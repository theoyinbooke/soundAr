//! Production orchestration for soundAr Video Studio.
//!
//! This is the single service used by native commands and Codex tools.  It owns
//! workflow admission/cancellation, but deliberately delegates persistence to
//! [`Store`] and deterministic media planning to the sibling video modules.

use super::{
    apply_dialogue_script, apply_lexicon, apply_timeline_edit, build_ass_document,
    build_audiogram_command, build_cover_image_command, build_loudness_analysis_command,
    build_podcast_audio_command, build_portrait_command_with_layout, build_proxy_command,
    build_report, build_thumbnail_command, build_timeline_render_plan, build_trailer_command,
    build_waveform_command, cover_spec, diff_spoken_words, discover_media_runtime,
    effective_entries, episode_transcript, ffmetadata_chapters, findings_for_dead_air,
    findings_for_loudness, findings_for_turn, identify_clip_candidates, instantiate_format,
    listen_to_episode, local_media_input_args, parse_loudness_analysis, plan_release,
    preflight_import_url_destination, probe_media, profile_dimensions, publish_atomic,
    sibling_staging_path, terminate_process_group, validate_import_url, write_ass_document_atomic,
    AdmissionOutcome, AssemblyOptions, CacheKeyBuilder, CacheStage, CandidatePolicy, CaptionTheme,
    CastMember, ClipModelPaths, DialogueScriptRequest, EpisodeListening, FfmpegProgressParser,
    GapReason, LayoutRole, LoudnessMeasurement, MediaError, MediaRuntimeStatus, Microseconds,
    NarrationBinding, NormalizedRect, PerformanceRecord, PortraitLayout, Provenance,
    ProvenanceKind, PublicHttpsProxy, PublicationState, QcReport, RationalFrameRate, RationalRate,
    ReleaseMemberKind, ReleaseMemberPlan, ReleasePlan, RenderArtifact, RenderArtifactRole,
    RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass, ResourceClass,
    ResourceRequest, ResourceScheduler, ReviewState, ReviewedScene, RevisionRecord, RevisionStage,
    RightsBasis, RightsConfirmation, RuntimeMediaProbe, ShowFormat, SourceAsset, SourceAssetKind,
    TakeFidelity, TimeRange, TimelineClip, TimelineGap, TimelineTrack, TrackKind, Validate,
    VideoEncoder, VideoError, VideoProjectManifest, VideoTimelineChangeReceipt,
    VideoTimelineEditRequest, VisualAsset, VisualEasing, VisualFit, VisualLayer, VisualMimeType,
    VisualMotion, CLIP_FPS, MAX_SHOW_FORMATS, MAX_VISUAL_ASSET_BYTES, MAX_VISUAL_DIMENSION,
    MAX_VISUAL_PIXELS, TRAILER_MAXIMUM_US, TRAILER_MINIMUM_US, TRAILER_TARGET_US,
};
use crate::store::Store;
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::{CString, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    mem::MaybeUninit,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// The track a performed line is placed on. Named rather than derived so dialogue never lands on
/// an imported source's audio track.
const DIALOGUE_TRACK_ID: &str = "dialogue";
/// The scene soundAr builds for a performed script. Named so it can be recognised later as the one
/// soundAr owns and may keep in step with the performance.
const DIALOGUE_SCENE_ID: &str = "dialogue-scene";

const SERVICE_VERSION: &str = "video-service-v1";
const PROJECT_LOCK_SECONDS: i64 = 120;
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 128 * 1024;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);
const COMMAND_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const RESOURCE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LINK_PREVIEW_TIMEOUT: Duration = Duration::from_secs(45);
const LINK_THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_THUMBNAIL_CAPTURE_BYTES: usize = 64 * 1024;
const THUMBNAIL_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "gif"];
const LINK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PACKAGE_AGGREGATE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_PACKAGE_ARCHIVE_BYTES: u64 = MAX_PACKAGE_AGGREGATE_BYTES + 64 * 1024 * 1024;
const PACKAGE_METADATA_RESERVE_BYTES: u64 = 64 * 1024 * 1024;
const DISK_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
/// A cover is one frame with no encode. Anything slower than this is a stuck process, not a
/// slow render.
const COVER_RENDER_TIMEOUT: Duration = Duration::from_secs(30);
/// One clip takes about a minute on a consumer card. Anything past this is a stuck process.
const CLIP_GENERATION_TIMEOUT: Duration = Duration::from_secs(900);
const CLIP_ENCODE_TIMEOUT: Duration = Duration::from_secs(120);
/// The canvas soundAr generates clips at: the largest that fits a 12 GB card with room for the
/// compute buffer, measured rather than assumed.
const CLIP_CANVAS: (u32, u32) = (864, 480);
/// Appended to every generated shot, so an episode's clips look like one piece of work.
const CLIP_STYLE: &str = "cinematic, shallow depth of field, natural light, film grain";
const SHAREABLE_FILE_MODE: u32 = 0o644;
const MAX_RENDER_DURATION: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_MEDIA_DURATION_US: i64 = 6 * 60 * 60 * 1_000_000;
const VISUAL_SOURCE_RECEIPT_TTL_MINUTES: i64 = 30;

pub type ServiceResult<T> = Result<T, VideoServiceError>;
pub type ProgressCallback = Arc<dyn Fn(VideoServiceProgress) + Send + Sync + 'static>;

/// Stable, transport-safe service error.  `code` is intentionally a string so
/// lower-level media/store codes remain lossless across Rust, Tauri and Codex.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VideoServiceError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl VideoServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: None,
        }
    }

    pub fn retryable(mut self, value: bool) -> Self {
        self.retryable = value;
        self
    }

    pub fn details(mut self, value: Value) -> Self {
        self.details = Some(value);
        self
    }

    fn cancelled() -> Self {
        Self::new("video.cancelled", "The video task was cancelled")
    }

    fn store(error: String) -> Self {
        let (code, message) = error
            .split_once(':')
            .filter(|(code, _)| code.starts_with("video."))
            .map(|(code, message)| (code.trim(), message.trim()))
            .unwrap_or(("video.store_failed", error.as_str()));
        let retryable = matches!(
            code,
            "video.project_locked" | "video.lock_lost" | "video.store_failed"
        );
        Self::new(code, message).retryable(retryable)
    }

    fn io(code: &'static str, message: &'static str, error: std::io::Error) -> Self {
        Self::new(code, message).details(json!({ "diagnostic": error.to_string() }))
    }

    pub fn stable_message(&self) -> String {
        format!("{}: {}", self.code, self.message)
    }
}

fn secure_private_directory(path: &Path) -> ServiceResult<()> {
    if let Err(error) = fs::DirBuilder::new()
        .mode(PRIVATE_DIRECTORY_MODE)
        .create(path)
    {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(VideoServiceError::io(
                "video.storage_unavailable",
                "A private managed directory could not be created",
                error,
            ));
        }
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            VideoServiceError::io(
                "video.unsafe_storage_path",
                "A managed storage path is not a safe directory",
                error,
            )
        })?;
    let metadata = directory.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.storage_unavailable",
            "A managed directory could not be inspected",
            error,
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != effective_uid {
        return Err(VideoServiceError::new(
            "video.unsafe_storage_path",
            "Managed directories must be owned by the current user and may not be symlinks",
        )
        .details(json!({
            "path": path,
            "owner_uid": metadata.uid(),
            "effective_uid": effective_uid,
        })));
    }
    directory
        .set_permissions(fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
        .map_err(|error| {
            VideoServiceError::io(
                "video.storage_permissions_failed",
                "Private permissions could not be applied to a managed directory",
                error,
            )
        })?;
    let secured = directory.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.storage_permissions_failed",
            "Managed directory permissions could not be verified",
            error,
        )
    })?;
    if secured.mode() & 0o7777 != PRIVATE_DIRECTORY_MODE || secured.uid() != effective_uid {
        return Err(VideoServiceError::new(
            "video.storage_permissions_failed",
            "Managed directory permissions did not remain private",
        )
        .details(json!({
            "path": path,
            "mode": format!("{:o}", secured.mode() & 0o7777),
        })));
    }
    Ok(())
}

fn secure_managed_directory_path(root: &Path, path: &Path) -> ServiceResult<()> {
    secure_private_directory(root)?;
    let relative = path.strip_prefix(root).map_err(|_| {
        VideoServiceError::new(
            "video.unsafe_storage_path",
            "A managed directory escaped Video Studio storage",
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(value) => current.push(value),
            Component::CurDir => continue,
            _ => {
                return Err(VideoServiceError::new(
                    "video.unsafe_storage_path",
                    "Managed directory paths may only contain normal components",
                ))
            }
        }
        secure_private_directory(&current)?;
    }
    Ok(())
}

fn secure_managed_file(path: &Path) -> ServiceResult<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            VideoServiceError::io(
                "video.unsafe_artifact_path",
                "A managed artifact is not a safe regular file",
                error,
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.artifact_not_found",
            "A managed artifact could not be inspected",
            error,
        )
    })?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != effective_uid || metadata.nlink() != 1 {
        return Err(VideoServiceError::new(
            "video.unsafe_artifact_path",
            "Managed artifacts must be single-link regular files owned by the current user",
        )
        .details(json!({
            "path": path,
            "owner_uid": metadata.uid(),
            "effective_uid": effective_uid,
            "link_count": metadata.nlink(),
        })));
    }
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
        .map_err(|error| {
            VideoServiceError::io(
                "video.storage_permissions_failed",
                "Private permissions could not be applied to a managed artifact",
                error,
            )
        })?;
    let secured = file.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.storage_permissions_failed",
            "Managed artifact permissions could not be verified",
            error,
        )
    })?;
    if secured.mode() & 0o7777 != PRIVATE_FILE_MODE || secured.uid() != effective_uid {
        return Err(VideoServiceError::new(
            "video.storage_permissions_failed",
            "Managed artifact permissions did not remain private",
        )
        .details(json!({
            "path": path,
            "mode": format!("{:o}", secured.mode() & 0o7777),
        })));
    }
    Ok(())
}

fn available_disk_bytes(path: &Path) -> ServiceResult<u64> {
    let encoded = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        VideoServiceError::new(
            "video.storage_unavailable",
            "The storage path contains an invalid null byte",
        )
    })?;
    let mut stats = MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::statvfs(encoded.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(VideoServiceError::io(
            "video.storage_unavailable",
            "Available disk space could not be inspected",
            std::io::Error::last_os_error(),
        ));
    }
    let stats = unsafe { stats.assume_init() };
    let fragment_size = if stats.f_frsize > 0 {
        stats.f_frsize as u64
    } else {
        stats.f_bsize as u64
    };
    Ok((stats.f_bavail as u64).saturating_mul(fragment_size))
}

fn ensure_disk_capacity(path: &Path, required_bytes: u64, operation: &str) -> ServiceResult<()> {
    let available_bytes = available_disk_bytes(path)?;
    if available_bytes < required_bytes {
        return Err(VideoServiceError::new(
            "video.insufficient_disk_space",
            "Video Studio needs more free disk space before this task can start",
        )
        .retryable(true)
        .details(json!({
            "operation": operation,
            "path": path,
            "required_bytes": required_bytes,
            "available_bytes": available_bytes,
        })));
    }
    Ok(())
}

fn with_disk_headroom(payload_bytes: u64, multiplier: u64) -> u64 {
    payload_bytes
        .saturating_mul(multiplier)
        .saturating_add(DISK_HEADROOM_BYTES)
}

fn validate_package_aggregate_bytes(aggregate_bytes: u64) -> ServiceResult<()> {
    if aggregate_bytes > MAX_PACKAGE_AGGREGATE_BYTES {
        return Err(VideoServiceError::new(
            "video.package_too_large",
            "Publish package contents exceed the 64 GiB local package limit",
        )
        .details(json!({
            "aggregate_bytes": aggregate_bytes,
            "maximum_bytes": MAX_PACKAGE_AGGREGATE_BYTES,
        })));
    }
    Ok(())
}

fn bounded_publish_package_bytes(master_bytes: u64) -> ServiceResult<u64> {
    let bounded_bytes = master_bytes.saturating_add(PACKAGE_METADATA_RESERVE_BYTES);
    if bounded_bytes > MAX_PACKAGE_AGGREGATE_BYTES {
        return Err(VideoServiceError::new(
            "video.package_too_large",
            "The final master leaves insufficient room for bounded publish metadata",
        )
        .details(json!({
            "master_bytes": master_bytes,
            "maximum_bytes": MAX_PACKAGE_AGGREGATE_BYTES,
            "reserved_metadata_bytes": PACKAGE_METADATA_RESERVE_BYTES,
        })));
    }
    Ok(bounded_bytes)
}

fn validate_source_size(size_bytes: u64) -> ServiceResult<()> {
    if size_bytes > MAX_SOURCE_BYTES {
        return Err(VideoServiceError::new(
            "video.source_too_large",
            "Video Studio supports individual source files up to 8 GiB",
        )
        .details(json!({
            "size_bytes": size_bytes,
            "maximum_bytes": MAX_SOURCE_BYTES,
        })));
    }
    Ok(())
}

fn validate_media_duration(duration_us: i64) -> ServiceResult<()> {
    if duration_us <= 0 || duration_us > MAX_MEDIA_DURATION_US {
        return Err(VideoServiceError::new(
            "video.duration_out_of_range",
            "Video Studio media duration must be positive and no longer than 6 hours",
        )
        .details(json!({
            "duration_us": duration_us,
            "maximum_duration_us": MAX_MEDIA_DURATION_US,
        })));
    }
    Ok(())
}

fn estimated_render_bytes(duration_us: i64, profile: RenderProfile) -> ServiceResult<u64> {
    validate_media_duration(duration_us)?;
    let seconds = u64::try_from(duration_us)
        .unwrap_or(0)
        .saturating_add(999_999)
        / 1_000_000;
    let bits_per_second = match profile {
        RenderProfile::Proxy => 8_000_000_u64,
        RenderProfile::Preview => 16_000_000_u64,
        RenderProfile::Final => 50_000_000_u64,
    };
    let encoded_bytes = seconds.saturating_mul(bits_per_second).saturating_add(7) / 8;
    Ok(encoded_bytes.saturating_add(256 * 1024 * 1024))
}

fn render_timeout(duration_us: Option<i64>, profile: RenderProfile) -> ServiceResult<Duration> {
    let duration_us = duration_us.unwrap_or(60_000_000);
    validate_media_duration(duration_us)?;
    let media_seconds = u64::try_from(duration_us)
        .unwrap_or(0)
        .saturating_add(999_999)
        / 1_000_000;
    let (minimum, multiplier, allowance) = match profile {
        RenderProfile::Proxy => (5 * 60, 3, 60),
        RenderProfile::Preview => (5 * 60, 4, 60),
        RenderProfile::Final => (10 * 60, 8, 120),
    };
    let seconds = media_seconds
        .saturating_mul(multiplier)
        .saturating_add(allowance)
        .max(minimum)
        .min(MAX_RENDER_DURATION.as_secs());
    Ok(Duration::from_secs(seconds))
}

/// Application-wide GPU admission request. The Video Studio keeps CPU, I/O,
/// and NVENC accounting in its deterministic local scheduler; RuntimeState can
/// inject one global gate so resident inference models and video workloads also
/// share the physical VRAM envelope.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharedGpuAdmissionRequest {
    pub job_id: String,
    pub project_id: String,
    pub resource_class: ResourceClass,
    pub requested_vram_mb: u32,
    pub requested_nvenc_sessions: u8,
    pub exclusive: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SharedGpuAdmissionWait {
    pub reason: String,
    #[serde(default = "default_gpu_retry_after_ms")]
    pub retry_after_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

fn default_gpu_retry_after_ms() -> u64 {
    RESOURCE_POLL_INTERVAL.as_millis() as u64
}

/// Marker for an application-owned GPU lease. Dropping the boxed value must
/// release its global reservation and wake waiters; the service guarantees the
/// lease is dropped on success, error, cancellation, and panic unwinding.
pub trait SharedGpuAdmissionLease: Send {}

pub enum SharedGpuAdmissionOutcome {
    Admitted(Box<dyn SharedGpuAdmissionLease>),
    Waiting(SharedGpuAdmissionWait),
}

impl SharedGpuAdmissionOutcome {
    pub fn admitted(lease: impl SharedGpuAdmissionLease + 'static) -> Self {
        Self::Admitted(Box::new(lease))
    }

    pub fn waiting(reason: impl Into<String>) -> Self {
        Self::Waiting(SharedGpuAdmissionWait {
            reason: reason.into(),
            retry_after_ms: default_gpu_retry_after_ms(),
            details: None,
        })
    }
}

/// Non-blocking global admission boundary. Implementations must atomically
/// return either a lease or normal backpressure. The service owns polling,
/// cancellation, and progress so no RuntimeState lock is held across a sleep.
pub trait SharedGpuAdmissionGate: Send + Sync {
    fn try_acquire(
        &self,
        request: &SharedGpuAdmissionRequest,
    ) -> ServiceResult<SharedGpuAdmissionOutcome>;
}

impl std::fmt::Display for VideoServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VideoServiceError {}

impl From<MediaError> for VideoServiceError {
    fn from(error: MediaError) -> Self {
        let code = if error.code.starts_with("video.") {
            error.code.clone()
        } else {
            format!("video.media.{}", error.code)
        };
        let mut result = Self::new(code, error.message).retryable(error.retryable);
        if let Some(detail) = error.detail {
            result.details = Some(json!({ "diagnostic": detail }));
        }
        result
    }
}

impl From<VideoError> for VideoServiceError {
    fn from(error: VideoError) -> Self {
        let mut result = Self::new(error.stable_code(), error.message).retryable(error.retryable);
        let mut details = error.details.unwrap_or_else(|| json!({}));
        if let Some(field) = error.field {
            if let Some(object) = details.as_object_mut() {
                object.insert("field".to_string(), Value::String(field));
            }
        }
        if details.as_object().is_some_and(|object| !object.is_empty()) {
            result.details = Some(details);
        }
        result
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VideoServiceProgress {
    pub job_id: String,
    pub project_id: String,
    pub phase: String,
    pub progress: f64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playable_artifact: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueuedVideoJob {
    pub job_id: String,
    pub project_id: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VideoJobResult {
    pub job_id: String,
    pub project_id: String,
    pub job: Value,
    pub project: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimelineEditServiceResult {
    pub project: Value,
    pub receipt: VideoTimelineChangeReceipt,
    pub job_id: String,
    pub replayed: bool,
}

/// One idempotent application of a cast and a written script to a durable project.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VideoScriptRequest {
    pub project_id: String,
    pub expected_revision: u64,
    /// Opaque Store version. The service owns its compare-and-swap check.
    pub base_version_id: String,
    /// Idempotency key for this exact cast and script.
    pub operation_id: String,
    pub cast: Vec<CastMember>,
    pub script: String,
    /// Proceed although some characters are cast on voices that cannot perform the cues written
    /// for them. The cues are removed rather than spoken; the writer has chosen to hear the lines
    /// without them.
    #[serde(default)]
    pub accept_dropped_cues: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VideoScriptReceipt {
    pub project_id: String,
    pub expected_revision: u64,
    pub base_version_id: String,
    pub operation_id: String,
    pub changed_paths: Vec<String>,
    pub invalidated_stages: BTreeSet<RevisionStage>,
    /// Turns whose words are unchanged. Their existing takes are still valid and must not be
    /// re-rendered; the caller queues speech only for `new_turn_ids`.
    pub retained_turn_ids: Vec<String>,
    pub new_turn_ids: Vec<String>,
    pub dropped_binding_ids: Vec<String>,
}

/// One deliverable soundAr produced and registered.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseMemberArtifact {
    pub kind: ReleaseMemberKind,
    pub artifact_id: String,
    pub managed_path: String,
    pub sha256: String,
    pub mime_type: String,
    pub duration_us: i64,
}

/// What a deliverable actually turned out to be, measured after publication.
#[derive(Clone, Debug, PartialEq)]
struct RenderedRelease {
    sha256: String,
    duration_us: i64,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseExportResult {
    pub project: Value,
    pub produced: Vec<ReleaseMemberArtifact>,
    /// Members that could not be produced, each naming its missing prerequisite. Reported rather
    /// than omitted, so a partial release is never mistaken for a complete one.
    pub skipped: Vec<ReleaseMemberPlan>,
    pub job_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptServiceResult {
    pub project: Value,
    pub receipt: VideoScriptReceipt,
    pub job_id: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VisualAssetOrigin {
    UserSelected {
        /// Opaque, short-lived receipt minted only by the native backend file chooser.
        receipt_id: String,
    },
    GeneratedLocally {
        /// Opaque receipt minted from an authenticated Codex image-generation result.
        receipt_id: String,
    },
}

impl VisualAssetOrigin {
    pub(crate) fn receipt_id(&self) -> &str {
        match self {
            Self::UserSelected { receipt_id } | Self::GeneratedLocally { receipt_id } => receipt_id,
        }
    }

    fn receipt_kind(&self) -> &'static str {
        match self {
            Self::UserSelected { .. } => "user_selected",
            Self::GeneratedLocally { .. } => "generated_locally",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizeVisualSelectionRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VisualSourceReceipt {
    pub id: String,
    pub receipt_kind: String,
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
    pub display_name: String,
    pub sha256: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustedGeneratedVisual {
    pub thread_id: String,
    pub turn_id: String,
    pub generation_id: String,
    pub source_path: PathBuf,
    pub producer_version: Option<String>,
    pub revised_prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AddVisualAssetRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
    pub operation_id: String,
    pub actor: String,
    pub origin: VisualAssetOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    pub range: TimeRange,
    pub fit: VisualFit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<super::NormalizedRect>,
    pub z_index: i16,
    pub motion: VisualMotion,
    pub transition_in_us: Microseconds,
    pub transition_out_us: Microseconds,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AddVisualAssetResult {
    pub project: Value,
    pub asset_id: String,
    pub layer_id: String,
    pub job_id: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CreateVideoProjectRequest {
    pub name: String,
    pub manifest: VideoProjectManifest,
    pub actor: String,
    /// Stored as the first revision reason.  This preserves prompt/audio intent
    /// without adding an out-of-contract field to the versioned manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_intent: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviseVideoManifestRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub manifest: VideoProjectManifest,
    pub actor: String,
    pub reason: String,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub invalidated_stages: BTreeSet<RevisionStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalImportRequest {
    pub project_id: String,
    pub source_path: PathBuf,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkRightsRequest {
    /// Must exactly match the canonical URL shown by `preview_link`.
    pub confirmed_url: String,
    pub basis: RightsBasis,
    pub statement: String,
    pub confirmed_by: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkImportRequest {
    pub project_id: String,
    pub url: String,
    pub actor: String,
    pub rights: LinkRightsRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinkPreview {
    pub canonical_url: String,
    pub provider: String,
    pub source_id: Option<String>,
    pub title: String,
    pub creator: Option<String>,
    pub duration_us: Option<i64>,
    pub thumbnail_url: Option<String>,
    /// Locally cached copy of `thumbnail_url`. The webview cannot load the remote address: the
    /// content security policy allows no external origins, so the poster has to come off disk.
    pub thumbnail_path: Option<String>,
    pub view_count: Option<u64>,
    pub upload_date: Option<String>,
    pub extractor: Option<String>,
    pub has_video: bool,
    pub has_audio: bool,
    pub rights_confirmation_required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortraitSourceLayout {
    CenterCrop,
    Contain,
    BlurPad,
}

impl From<PortraitSourceLayout> for PortraitLayout {
    fn from(value: PortraitSourceLayout) -> Self {
        match value {
            PortraitSourceLayout::CenterCrop => PortraitLayout::CenterCrop,
            PortraitSourceLayout::Contain => PortraitLayout::Contain,
            PortraitSourceLayout::BlurPad => PortraitLayout::BlurPad,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortraitRenderRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_asset_id: Option<String>,
    pub profile: RenderProfile,
    pub layout: PortraitSourceLayout,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub variation: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishPackageRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_dir: Option<PathBuf>,
    pub actor: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimelineRenderProfile {
    Preview,
    Final,
}

impl From<TimelineRenderProfile> for RenderProfile {
    fn from(value: TimelineRenderProfile) -> Self {
        match value {
            TimelineRenderProfile::Preview => RenderProfile::Preview,
            TimelineRenderProfile::Final => RenderProfile::Final,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimelineRenderRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
    pub profile: TimelineRenderProfile,
    pub caption_theme: CaptionTheme,
    pub portrait_layout: PortraitSourceLayout,
    pub actor: String,
    /// Deterministic variation discriminator. Zero is the canonical master;
    /// positive values are non-primary alternates with independent cache keys.
    #[serde(default)]
    pub variation: u16,
    #[serde(default = "default_true")]
    pub include_title_cards: bool,
    #[serde(default = "default_true")]
    pub include_speaker_cards: bool,
    #[serde(default = "default_true")]
    pub burn_captions: bool,
}

/// Renders multiple deterministic alternates from one frozen editorial
/// manifest, then publishes every output against one resulting version.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimelineRenderBatchRequest {
    pub base: TimelineRenderRequest,
    pub variations: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NarrationReplacement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    /// Whether this take is the finished performance or a fast stand-in. Draft takes let a whole
    /// episode be heard quickly; only the lines that survive that listen are re-read for real.
    #[serde(default)]
    pub fidelity: TakeFidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    /// Target one dialogue turn. Turn-scoped replacement is the multi-character path: it
    /// re-reads exactly one line and leaves every other take in the scene untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
    pub history_id: String,
    pub voice_id: String,
    pub model_id: String,
    pub speaker: String,
    pub language: String,
    /// How the take was performed, recorded on its binding so a later change of persona,
    /// direction, or engine vocabulary re-reads exactly the lines it affects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<PerformanceRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplaceNarrationRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
    pub actor: String,
    /// Durable owning workflow id. RuntimeState uses this to adopt an already
    /// queued replacement after a crash between child creation and checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_job_id: Option<String>,
    pub replacements: Vec<NarrationReplacement>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableLinkImportRequest {
    request: LinkImportRequest,
    canonical_url: String,
    rights_confirmation: RightsConfirmation,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableLocalImportOrigin {
    #[default]
    UserUpload,
    SoundArHistory {
        history_id: String,
        generation_job_id: String,
        generation_kind: String,
        model_id: String,
        voice: String,
        engine: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableLocalImportRequest {
    project_id: String,
    source_path: PathBuf,
    actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default)]
    origin: DurableLocalImportOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_job_id: Option<String>,
    #[serde(default = "default_normal_priority")]
    priority: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectExpectation {
    revision: i64,
    version_id: String,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SingleRenderTestFailpoint {
    PortraitBeforeAtomicPublication,
    PortraitAfterAtomicPublication,
    TimelineBeforeAtomicPublication,
    TimelineAfterAtomicPublication,
}

fn default_true() -> bool {
    true
}

fn default_normal_priority() -> String {
    "normal".to_string()
}

/// Shared service instance.  Put this behind one `Arc` in RuntimeState and pass
/// that same instance to every transport adapter.
pub struct VideoStudioService {
    store: Arc<Store>,
    video_root: PathBuf,
    scheduler: Mutex<ResourceScheduler>,
    storage_reservations: Mutex<StorageReservationState>,
    gpu_admission_gate: Option<Arc<dyn SharedGpuAdmissionGate>>,
    cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>,
    runtime_cache: Mutex<Option<(Instant, MediaRuntimeStatus)>>,
    #[cfg(test)]
    package_test_barrier: Mutex<Option<Arc<std::sync::Barrier>>>,
    #[cfg(test)]
    local_import_test_barrier: Mutex<Option<Arc<std::sync::Barrier>>>,
    #[cfg(test)]
    single_render_test_failpoint: Mutex<Option<SingleRenderTestFailpoint>>,
}

impl VideoStudioService {
    /// Builds a complete, self-describing publish package.  The operation is
    /// synchronous for CLI/Codex ergonomics but still owns a durable job and
    /// workflow checkpoint; Tauri callers should invoke it off the UI thread.
    pub fn export_publish_package(
        &self,
        mut request: PublishPackageRequest,
    ) -> ServiceResult<Value> {
        let project = self.get_project(&request.project_id)?;
        let expectation = expectation_from_optional(
            &project,
            request.expected_revision,
            request.expected_version_id.as_deref(),
        )?;
        request.expected_revision = Some(expectation.revision);
        request.expected_version_id = Some(expectation.version_id);
        let durable = serde_json::to_value(&request).map_err(json_error)?;
        let job_id = self
            .store
            .create_job("video_publish_package", &durable)
            .map_err(VideoServiceError::store)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.clone(), Arc::clone(&cancel));
        let _cancellation_registration = CancellationRegistration {
            cancellations: &self.cancellations,
            job_id: job_id.clone(),
        };
        let status = self
            .store
            .start_job(&job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let result = self.perform_publish_package(&job_id, &request, project, cancel.as_ref());
        match result {
            Ok(result) => {
                if cancel.load(Ordering::Acquire) {
                    let _ = self.store.cancel_job(&job_id);
                    return Err(VideoServiceError::cancelled());
                }
                let completed = self
                    .store
                    .complete_job(&job_id)
                    .map_err(VideoServiceError::store)?;
                if !completed {
                    let job = self
                        .store
                        .get_job(&job_id)
                        .map_err(VideoServiceError::store)?;
                    return if job
                        .as_ref()
                        .and_then(|job| job.get("status"))
                        .and_then(Value::as_str)
                        == Some("cancelled")
                    {
                        Err(VideoServiceError::cancelled())
                    } else {
                        Err(VideoServiceError::new(
                            "video.job_state_failed",
                            "The publish package finished locally but its durable completion was rejected",
                        )
                        .retryable(true)
                        .details(json!({ "job": job })))
                    };
                }
                Ok(json!({
                    "job_id": job_id,
                    "project_id": request.project_id,
                    "output": result.output,
                    "package_path": result.package_path,
                    "archive_path": result.archive_path,
                    "export_path": result.export_path,
                }))
            }
            Err(error) if error.code == "video.cancelled" => {
                self.store
                    .cancel_job(&job_id)
                    .map_err(VideoServiceError::store)?;
                Err(error)
            }
            Err(error) => match self.store.fail_job(&job_id, &error.stable_message()) {
                Ok(()) => Err(error),
                Err(store_error) => Err(VideoServiceError::new(
                    "video.job_state_failed",
                    "Publish failed and its durable failure state could not be saved",
                )
                .retryable(true)
                .details(json!({
                    "operation_error": error,
                    "store_error": store_error,
                }))),
            },
        }
    }

    fn perform_publish_package(
        &self,
        job_id: &str,
        request: &PublishPackageRequest,
        project: Value,
        cancel: &AtomicBool,
    ) -> ServiceResult<PublishedPackageResult> {
        let _lease = self.acquire_resources(
            job_id,
            &request.project_id,
            ResourceRequest::light(),
            cancel,
            None,
        )?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        let expectation = expectation_from_optional(
            &project,
            request.expected_revision,
            request.expected_version_id.as_deref(),
        )?;
        let outputs = project
            .get("outputs")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_store_shape("outputs"))?;
        let master = outputs
            .iter()
            .find(|output| {
                output.get("version_id").and_then(Value::as_str)
                    == Some(expectation.version_id.as_str())
                    && output.get("status").and_then(Value::as_str) == Some("ready")
                    && output.get("kind").and_then(Value::as_str) == Some("master")
                    && output.get("is_primary").and_then(Value::as_bool) == Some(true)
                    && output.get("mime_type").and_then(Value::as_str) == Some("video/mp4")
            })
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.final_master_required",
                    "Render a final master for the current timeline before exporting a publish package",
                )
            })?;
        let master_path = PathBuf::from(value_string(master, "artifact_path")?);
        let master_path = self.resolve_absolute_managed_path(&master_path)?;
        let master_size = fs::metadata(&master_path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.package_failed",
                    "The final master could not be inspected for packaging",
                    error,
                )
            })?
            .len();
        let bounded_package_bytes = bounded_publish_package_bytes(master_size)?;
        // Package construction writes an independent package master and ZIP.
        // Reserve both before the first potentially multi-gigabyte integrity
        // pass so concurrent workers cannot all spend the same free bytes.
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:publish-package"),
            &self.video_root,
            with_disk_headroom(bounded_package_bytes, 2),
            "publish_package",
        )?;
        let master_sha = value_string(master, "sha256")?;
        let version = project
            .get("version")
            .ok_or_else(|| invalid_store_shape("version"))?;
        let version_id = value_string(version, "id")?;
        let version_sha = value_string(version, "sha256")?;
        let master_output_id = value_string(master, "id")?;
        if version_id != expectation.version_id {
            return Err(VideoServiceError::new(
                "video.revision_integrity_failed",
                "The package project version does not match its current version identity",
            ));
        }
        let mut package_manifest_slice = manifest_content_value(&manifest)?;
        package_manifest_slice
            .as_object_mut()
            .ok_or_else(|| invalid_store_shape("manifest"))?
            // Published/preview artifacts are derived products. Excluding them
            // allows a safely resumed package job to reuse the same archive,
            // while every editorial field remains bound into the cache key.
            .remove("render_artifacts");
        let cache_key = CacheKeyBuilder::new(CacheStage::PublishPackage, SERVICE_VERSION)
            .artifact("final_master", master_sha.clone())
            .manifest_slice(package_manifest_slice)
            .profile(json!({
                "format": "publish-zip-v3",
                "version_id": version_id,
                "version_sha256": version_sha,
                "master_output_id": master_output_id,
            }))
            .build()?
            .into_string();
        // Construct every metadata payload and enforce the exact aggregate
        // before reading or copying a potentially 64 GiB master.
        self.build_publish_package_payloads(
            &manifest,
            &project,
            master,
            master_size,
            &master_sha,
            &cache_key,
        )?;
        if sha256_file_with_cancel(&master_path, Some(cancel))? != master_sha {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "The final master checksum no longer matches its output record",
            ));
        }
        self.checkpoint_stage(
            &request.project_id,
            project
                .get("version")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str),
            "publish_package",
            "master",
            job_id,
            "running",
            "light",
            0.05,
            &cache_key,
            None,
            json!({ "master_output_id": master.get("id") }),
            None,
        )?;
        #[cfg(test)]
        if let Some(barrier) = self
            .package_test_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            // One-shot deterministic boundary for the direct synchronous API
            // cancellation regression. Production builds contain no hook.
            barrier.wait();
            barrier.wait();
        }
        self.ensure_not_cancelled(cancel)?;
        let package_parent = self.project_dir(&request.project_id)?.join("packages");
        self.secure_managed_directory(&package_parent)?;
        let package_path = package_parent.join(format!("publish-{}", &cache_key[..16]));
        if package_path.exists() {
            self.secure_managed_flat_directory(&package_path)?;
        }
        let package_is_valid =
            match validate_package_directory_with_cancel(&package_path, Some(cancel)) {
                Ok(()) => true,
                Err(error) if error.code == "video.cancelled" => return Err(error),
                Err(_) => false,
            };
        if !package_is_valid {
            if package_path.exists() {
                return Err(VideoServiceError::new(
                    "video.package_invalid",
                    "A conflicting managed package directory already exists",
                ));
            }
            ensure_disk_capacity(
                &package_parent,
                with_disk_headroom(master_size, 2),
                "publish_package",
            )?;
            let staging = package_parent.join(format!(
                ".publish-{}.{}.partial",
                &cache_key[..16],
                new_id()
            ));
            self.secure_managed_directory(&staging)?;
            let build_result = (|| -> ServiceResult<()> {
                self.write_publish_package(
                    &staging,
                    &manifest,
                    &project,
                    master,
                    &master_path,
                    &master_sha,
                    &cache_key,
                    cancel,
                )?;
                // Enforce the aggregate contract while the package is still
                // disposable. A master just below 64 GiB can cross the limit
                // once manifests/captions are added; it must never be renamed
                // into durable managed storage in that state.
                package_member_inventory(&staging, Some(cancel))?;
                sync_directory_tree(&staging)
            })();
            if let Err(error) = build_result {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            fs::rename(&staging, &package_path).map_err(|error| {
                let _ = fs::remove_dir_all(&staging);
                VideoServiceError::io(
                    "video.package_publish_failed",
                    "The publish package could not be atomically published",
                    error,
                )
            })?;
            sync_directory(&package_parent)?;
            self.secure_managed_flat_directory(&package_path)?;
        }
        validate_package_directory_with_cancel(&package_path, Some(cancel))?;
        validate_package_identity(
            &package_path,
            &cache_key,
            &master_sha,
            &version_id,
            &version_sha,
            &master_output_id,
            Some(cancel),
        )?;
        let archive_path = package_parent.join(format!("publish-{}.zip", &cache_key[..16]));
        if !archive_path.exists() {
            let package_bytes = package_regular_files_within_limit(&package_path, Some(cancel))?
                .iter()
                .fold(0_u64, |total, (_, _, size)| total.saturating_add(*size));
            ensure_disk_capacity(
                &package_parent,
                with_disk_headroom(package_bytes, 1),
                "publish_package_archive",
            )?;
        }
        write_publish_zip_atomic(&package_path, &archive_path, cancel)?;
        validate_publish_zip_with_cancel(&archive_path, &package_path, Some(cancel))?;
        let archive_sha = sha256_file_with_cancel(&archive_path, Some(cancel))?;
        // A publish package is an output of the current timeline, not an edit
        // to it. Registering it in Store keeps Projects/History/assistant parity
        // without advancing the manifest and immediately making its master
        // stale. The managed ZIP remains content-addressed and durable.
        ensure_project_matches(&self.get_project(&request.project_id)?, &expectation)?;
        self.store
            .put_video_cache(
                &cache_key,
                "publish_package",
                Some(&request.project_id),
                &json!({
                    "master_sha256": master_sha,
                    "master_output_id": master_output_id,
                    "version_id": version_id,
                    "version_sha256": version_sha,
                }),
                &archive_path,
            )
            .map_err(VideoServiceError::store)?;
        let committed_expectation = expectation;
        let export_path = if let Some(destination) = &request.destination_dir {
            ensure_project_matches(
                &self.get_project(&request.project_id)?,
                &committed_expectation,
            )?;
            let export_bytes = package_regular_files_within_limit(&package_path, Some(cancel))?
                .iter()
                .fold(0_u64, |total, (_, _, size)| total.saturating_add(*size));
            let _export_storage_lease = self.reserve_storage(
                format!("{job_id}:publish-export"),
                destination,
                with_disk_headroom(export_bytes, 1),
                "publish_package_export",
            )?;
            let path = export_package_directory(
                &package_path,
                destination,
                &manifest.name,
                &cache_key,
                cancel,
                || {
                    let lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
                    ensure_project_matches(
                        &self.get_project(&request.project_id)?,
                        &committed_expectation,
                    )?;
                    Ok(lock)
                },
            )?;
            Some(path)
        } else {
            None
        };
        // Final registration is a short CAS-critical section. Expensive ZIP
        // and optional user export work happens before this fresh lease; no
        // non-primary package output can attach after the editorial version
        // changes merely because Store only guards primary outputs.
        let publication_lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
        let committed = self.get_project(&request.project_id)?;
        ensure_project_matches(&committed, &committed_expectation)?;
        let version_id = Some(committed_expectation.version_id.as_str());
        let output_id = stable_output_id(
            &request.project_id,
            committed_expectation.revision,
            "publish-package",
            &cache_key,
            0,
        );
        let output = self
            .store
            .publish_video_output_current_cancellable(
                &json!({
                    "id": output_id,
                    "project_id": request.project_id,
                    "version_id": version_id,
                    "job_id": job_id,
                    "kind": "publish-package",
                    "label": "Publish package",
                    "artifact_path": archive_path,
                    "mime_type": "application/zip",
                    "sha256": archive_sha,
                    "is_primary": false,
                    "provenance": {
                        "producer": "soundAr Video Studio",
                        "producer_version": SERVICE_VERSION,
                        "package_dir": package_path,
                        "archive_path": archive_path,
                        "master_sha256": master_sha,
                        "master_output_id": master_output_id,
                        "version_id": version_id,
                        "version_sha256": version_sha,
                        "cache_key": cache_key,
                        "export_path": export_path,
                    },
                }),
                committed_expectation.revision,
                &committed_expectation.version_id,
                &publication_lock.token,
                cancel,
            )
            .map_err(VideoServiceError::store)?;
        ensure_project_matches(
            &self.get_project(&request.project_id)?,
            &committed_expectation,
        )?;
        drop(publication_lock);
        self.checkpoint_stage(
            &request.project_id,
            version_id,
            "publish_package",
            "master",
            job_id,
            "completed",
            "light",
            1.0,
            &cache_key,
            Some(&archive_sha),
            json!({
                "output_id": output.get("id"),
                "package_path": package_path,
                "archive_path": archive_path,
            }),
            None,
        )?;
        Ok(PublishedPackageResult {
            output,
            package_path,
            archive_path,
            export_path,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_publish_package_payloads(
        &self,
        manifest: &VideoProjectManifest,
        project: &Value,
        master: &Value,
        master_size: u64,
        master_sha: &str,
        cache_key: &str,
    ) -> ServiceResult<PublishPackagePayloads> {
        let (publish_manifest, query_parameters_redacted) = redact_publish_manifest_urls(manifest);
        let timeline_manifest = serde_json::to_vec_pretty(&publish_manifest).map_err(json_error)?;
        let captions = if manifest.captions.is_empty() {
            None
        } else {
            Some(captions_to_srt(manifest)?.into_bytes())
        };
        let readme = format!(
            "soundAr publish package\n\nProject: {}\nGenerated: {}\n\nContents\n- master.mp4: final assembled video\n- timeline-manifest.json: versioned edit source of truth\n- package-manifest.json: checksums, provenance and rights receipts\n{}{}\nThis package was rendered locally. Verify destination-specific publishing requirements before upload.\n",
            manifest.name,
            manifest.updated_at,
            if captions.is_none() {
                ""
            } else {
                "- captions.srt: timed captions\n"
            },
            if query_parameters_redacted {
                "\nPrivacy: secret-bearing source URL query parameters were removed from exported JSON. Exact authorized URLs remain cryptographically bound by their SHA-256 receipts.\n"
            } else {
                ""
            },
        )
        .into_bytes();
        let mut files = vec![json!({
            "path": "master.mp4",
            "size_bytes": master_size,
            "sha256": master_sha,
        })];
        files.push(json!({
            "path": "timeline-manifest.json",
            "size_bytes": timeline_manifest.len(),
            "sha256": sha256_bytes(&timeline_manifest),
        }));
        if let Some(captions) = captions.as_ref() {
            files.push(json!({
                "path": "captions.srt",
                "size_bytes": captions.len(),
                "sha256": sha256_bytes(captions),
            }));
        }
        files.push(json!({
            "path": "README.txt",
            "size_bytes": readme.len(),
            "sha256": sha256_bytes(&readme),
        }));
        let package_manifest_value = json!({
            "schema_version": 1,
            "kind": "soundar_publish_package",
            "project": {
                "id": manifest.project_id,
                "name": manifest.name,
                "manifest_revision": manifest.revision,
                "store_revision": project.get("revision"),
                "version": project.get("version"),
            },
            "generated_at": manifest.updated_at,
            "producer": { "name": "soundAr Video Studio", "version": SERVICE_VERSION },
            "cache_key": cache_key,
            "privacy": {
                "query_parameters_redacted": query_parameters_redacted,
                "exact_source_urls_bound_by_sha256": true,
            },
            "master": {
                "output_id": master.get("id"),
                "sha256": master_sha,
                "duration_us": master.get("duration_us"),
                "width": master.get("width"),
                "height": master.get("height"),
                "mime_type": "video/mp4",
            },
            "files": files,
            "rights_confirmations": publish_manifest.rights_confirmations,
            "source_provenance": publish_manifest.source_assets.iter().map(|asset| json!({
                "source_asset_id": asset.id,
                "sha256": asset.sha256,
                "provenance": asset.provenance,
                "rights_confirmation_id": asset.rights_confirmation_id,
            })).collect::<Vec<_>>(),
        });
        let package_manifest =
            serde_json::to_vec_pretty(&package_manifest_value).map_err(json_error)?;
        if package_manifest.len() > MAX_CAPTURE_BYTES {
            return Err(VideoServiceError::new(
                "video.package_too_large",
                "The publish package metadata exceeds its bounded manifest limit",
            ));
        }
        let aggregate_bytes = master_size
            .saturating_add(timeline_manifest.len() as u64)
            .saturating_add(captions.as_ref().map_or(0, |value| value.len() as u64))
            .saturating_add(readme.len() as u64)
            .saturating_add(package_manifest.len() as u64);
        validate_package_aggregate_bytes(aggregate_bytes)?;
        Ok(PublishPackagePayloads {
            timeline_manifest,
            captions,
            readme,
            package_manifest,
            aggregate_bytes,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn write_publish_package(
        &self,
        staging: &Path,
        manifest: &VideoProjectManifest,
        project: &Value,
        master: &Value,
        master_path: &Path,
        master_sha: &str,
        cache_key: &str,
        cancel: &AtomicBool,
    ) -> ServiceResult<()> {
        let master_size = fs::metadata(master_path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.package_failed",
                    "The final master could not be inspected for packaging",
                    error,
                )
            })?
            .len();
        let payloads = self.build_publish_package_payloads(
            manifest,
            project,
            master,
            master_size,
            master_sha,
            cache_key,
        )?;
        let packaged_master = staging.join("master.mp4");
        copy_file_verified(
            master_path,
            &packaged_master,
            CopiedFileVisibility::ManagedPrivate,
            cancel,
        )?;
        write_new_file(
            &staging.join("timeline-manifest.json"),
            &payloads.timeline_manifest,
        )?;
        if let Some(captions) = payloads.captions.as_ref() {
            write_new_file(&staging.join("captions.srt"), captions)?;
        }
        write_new_file(&staging.join("README.txt"), &payloads.readme)?;
        write_new_file(
            &staging.join("package-manifest.json"),
            &payloads.package_manifest,
        )?;
        debug_assert!(payloads.aggregate_bytes <= MAX_PACKAGE_AGGREGATE_BYTES);
        Ok(())
    }
}

struct ProjectLock<'a> {
    service: &'a VideoStudioService,
    project_id: String,
    token: String,
}

impl<'a> ProjectLock<'a> {
    fn acquire(
        service: &'a VideoStudioService,
        project_id: &str,
        actor: &str,
    ) -> ServiceResult<Self> {
        let owner = format!("{}:{}", truncate_chars(actor, 80), new_id());
        let lock = service
            .store
            .acquire_video_project_lock(project_id, &owner, PROJECT_LOCK_SECONDS)
            .map_err(VideoServiceError::store)?;
        Ok(Self {
            service,
            project_id: project_id.to_string(),
            token: value_string(&lock, "token")?,
        })
    }
}

impl Drop for ProjectLock<'_> {
    fn drop(&mut self) {
        let _ = self
            .service
            .store
            .release_video_project_lock(&self.project_id, &self.token);
    }
}

struct SchedulerLease<'a> {
    scheduler: &'a Mutex<ResourceScheduler>,
    job_id: String,
    shared_gpu_lease: Option<Box<dyn SharedGpuAdmissionLease>>,
}

impl Drop for SchedulerLease<'_> {
    fn drop(&mut self) {
        // Release the application-wide GPU reservation before returning local
        // capacity. This prevents another video worker from observing free
        // local VRAM while the global reservation is still live.
        self.shared_gpu_lease.take();
        let _ = self
            .scheduler
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .release(&self.job_id);
    }
}

#[derive(Default)]
struct StorageReservationState {
    active: HashMap<String, StorageReservation>,
}

#[derive(Clone, Copy)]
struct StorageReservation {
    device_id: u64,
    bytes: u64,
}

struct StorageLease<'a> {
    reservations: &'a Mutex<StorageReservationState>,
    id: String,
}

impl Drop for StorageLease<'_> {
    fn drop(&mut self) {
        self.reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .remove(&self.id);
    }
}

struct CancellationRegistration<'a> {
    cancellations: &'a Mutex<HashMap<String, Arc<AtomicBool>>>,
    job_id: String,
}

impl Drop for CancellationRegistration<'_> {
    fn drop(&mut self) {
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.job_id);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalSourceIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

struct ResolvedVisualSource {
    path: PathBuf,
    expected_identity: LocalSourceIdentity,
    expected_sha256: String,
    inspection: ImageInspection,
    provenance: Provenance,
}

impl LocalSourceIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

struct PreparedManagedFile {
    path: PathBuf,
    relative_path: String,
    sha256: String,
    probe: RuntimeMediaProbe,
}

struct ManagedSource {
    id: String,
    path: PathBuf,
    relative_path: String,
    sha256: String,
    probe: RuntimeMediaProbe,
    kind: SourceAssetKind,
    provenance: Provenance,
    rights: Option<RightsConfirmation>,
}

struct DerivedProduct {
    id: String,
    kind: String,
    path: PathBuf,
    sha256: String,
    cache_key: String,
    mime_type: String,
    role: RenderArtifactRole,
    duration_us: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
    probe: Value,
}

impl DerivedProduct {
    fn playable_value(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "artifact_path": self.path,
            "mime_type": self.mime_type,
            "sha256": self.sha256,
            "duration_us": self.duration_us,
            "width": self.width,
            "height": self.height,
        })
    }
}

struct PublishedPackageResult {
    output: Value,
    package_path: PathBuf,
    archive_path: PathBuf,
    export_path: Option<PathBuf>,
}

struct PublishPackagePayloads {
    timeline_manifest: Vec<u8>,
    captions: Option<Vec<u8>>,
    readme: Vec<u8>,
    package_manifest: Vec<u8>,
    aggregate_bytes: u64,
}

struct PreparedNarrationReplacement {
    clip_id: String,
    /// Set when the line has never been performed, so the commit appends this clip instead of
    /// rewriting one.
    new_clip: Option<TimelineClip>,
    replaced_binding_id: Option<String>,
    artifact: RenderArtifact,
    binding: NarrationBinding,
}

struct PreparedTimelineRender {
    request: TimelineRenderRequest,
    caption_artifact: RenderArtifact,
    render_artifact: RenderArtifact,
    output_path: PathBuf,
    output_sha: String,
    output_duration_us: i64,
    width: u32,
    height: u32,
    caption_key: String,
    render_key: String,
    stage_key: String,
    scope_key: String,
    resource_class: String,
    caption_cache_hit: bool,
    render_cache_hit: bool,
    wall_seconds: f64,
    scene_count: usize,
    nvenc_with_software_fallback: bool,
}

enum RenderRunError {
    Cancelled(VideoServiceError),
    Failed {
        error: VideoServiceError,
        stderr: String,
    },
}

struct StagedRenderCleanup {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl StagedRenderCleanup {
    fn for_plan(plan: &RenderCommandPlan) -> Self {
        let mut paths = vec![plan.primary.output.clone()];
        if let Some(fallback) = &plan.software_fallback {
            if !paths.contains(&fallback.output) {
                paths.push(fallback.output.clone());
            }
        }
        Self { paths, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedRenderCleanup {
    fn drop(&mut self) {
        if self.armed {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[derive(Debug)]
struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug)]
struct BoundedPipeCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Clone, Debug)]
struct CommandOutputQuota {
    directory: PathBuf,
    prefix: String,
    max_file_bytes: u64,
    max_aggregate_bytes: u64,
}

fn run_captured_command(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    max_stdout_bytes: usize,
    proxy_url: Option<&str>,
    output_quota: Option<&CommandOutputQuota>,
) -> ServiceResult<CapturedOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(proxy_url) = proxy_url {
        // yt-dlp receives the same URL explicitly. Setting every conventional
        // proxy variable also confines any JavaScript-runtime or downloader
        // descendants it launches; clearing NO_PROXY prevents a private
        // redirect from bypassing the authenticated CONNECT-only proxy.
        command
            .env("HTTP_PROXY", proxy_url)
            .env("http_proxy", proxy_url)
            .env("HTTPS_PROXY", proxy_url)
            .env("https_proxy", proxy_url)
            .env("ALL_PROXY", proxy_url)
            .env("all_proxy", proxy_url);
    }
    // yt-dlp and its FFmpeg merger create only private files inside the
    // owner-only managed source directory, including transient fragments.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o077);
            let file_size_limit = libc::rlimit {
                rlim_cur: MAX_SOURCE_BYTES as libc::rlim_t,
                rlim_max: MAX_SOURCE_BYTES as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &file_size_limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        VideoServiceError::io(
            "video.command_start_failed",
            "The local media command could not be started",
            error,
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        VideoServiceError::new(
            "video.command_start_failed",
            "The local media command has no output stream",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        VideoServiceError::new(
            "video.command_start_failed",
            "The local media command has no diagnostic stream",
        )
    })?;
    if let Err(error) =
        set_capture_pipe_nonblocking(&stdout).and_then(|_| set_capture_pipe_nonblocking(&stderr))
    {
        stop_captured_process_group(&mut child);
        return Err(VideoServiceError::io(
            "video.command_read_failed",
            "The local media command output could not be secured",
            error,
        ));
    }
    let process_group = i32::try_from(child.id()).ok();
    let stop_readers = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_bounded_capture_reader(
        "soundar-video-command-stdout",
        stdout,
        max_stdout_bytes,
        Arc::clone(&stop_readers),
    )
    .map_err(|error| {
        stop_captured_process_group(&mut child);
        VideoServiceError::io(
            "video.command_read_failed",
            "The local media command output reader could not be started",
            error,
        )
    })?;
    let stderr_reader = match spawn_bounded_capture_reader(
        "soundar-video-command-stderr",
        stderr,
        MAX_DIAGNOSTIC_BYTES,
        Arc::clone(&stop_readers),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            stop_captured_process_group(&mut child);
            stop_readers.store(true, Ordering::Release);
            let _ = stdout_reader.join();
            return Err(VideoServiceError::io(
                "video.command_read_failed",
                "The local media command diagnostic reader could not be started",
                error,
            ));
        }
    };
    let started = Instant::now();
    let status = loop {
        if let Some(quota) = output_quota {
            if let Err(error) = validate_command_output_quota(quota) {
                stop_captured_process_group(&mut child);
                let _ = finish_captured_readers(
                    stdout_reader,
                    stderr_reader,
                    stop_readers,
                    process_group,
                );
                return Err(error);
            }
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            stop_captured_process_group(&mut child);
            let _ =
                finish_captured_readers(stdout_reader, stderr_reader, stop_readers, process_group);
            return Err(VideoServiceError::cancelled());
        }
        if started.elapsed() > timeout {
            stop_captured_process_group(&mut child);
            let _ =
                finish_captured_readers(stdout_reader, stderr_reader, stop_readers, process_group);
            return Err(VideoServiceError::new(
                "video.command_timeout",
                "The local media command timed out",
            )
            .retryable(true));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                stop_captured_process_group(&mut child);
                let _ = finish_captured_readers(
                    stdout_reader,
                    stderr_reader,
                    stop_readers,
                    process_group,
                );
                return Err(VideoServiceError::io(
                    "video.command_monitor_failed",
                    "The local media command could not be monitored",
                    error,
                ));
            }
        }
        thread::sleep(Duration::from_millis(20));
    };
    let (stdout, stderr, fully_drained) =
        finish_captured_readers(stdout_reader, stderr_reader, stop_readers, process_group)?;
    if !fully_drained {
        return Err(VideoServiceError::new(
            "video.command_pipe_open",
            "The local media command left its output pipes open after exiting",
        )
        .retryable(true));
    }
    if let Some(quota) = output_quota {
        validate_command_output_quota(quota)?;
    }
    if stdout.truncated {
        return Err(VideoServiceError::new(
            "video.command_output_too_large",
            "The local media command returned too much data",
        ));
    }
    Ok(CapturedOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn validate_command_output_quota(quota: &CommandOutputQuota) -> ServiceResult<()> {
    let mut aggregate_bytes = 0_u64;
    for entry in fs::read_dir(&quota.directory).map_err(|error| {
        VideoServiceError::io(
            "video.link_import_failed",
            "The download staging directory could not be monitored",
            error,
        )
    })? {
        let entry = entry.map_err(|error| {
            VideoServiceError::io(
                "video.link_import_failed",
                "A download staging entry could not be monitored",
                error,
            )
        })?;
        let matches = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&quota.prefix));
        if !matches {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(VideoServiceError::io(
                    "video.link_import_failed",
                    "A download staging entry could not be inspected",
                    error,
                ))
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.unsafe_artifact_path",
                "Downloaded media staging may only contain regular files",
            ));
        }
        let size = metadata.len();
        // RLIMIT_FSIZE prevents the child and all descendants from crossing
        // this boundary. Treat reaching it as a quota failure too, so a helper
        // terminated by SIGXFSZ reports the stable product error.
        if size >= quota.max_file_bytes {
            return Err(VideoServiceError::new(
                "video.source_too_large",
                "The authorized download reached the 8 GiB source limit",
            )
            .details(json!({
                "file_bytes": size,
                "maximum_bytes": quota.max_file_bytes,
            })));
        }
        aggregate_bytes = aggregate_bytes.saturating_add(size);
        if aggregate_bytes > quota.max_aggregate_bytes {
            return Err(VideoServiceError::new(
                "video.source_too_large",
                "The authorized download exceeded its bounded staging quota",
            )
            .details(json!({
                "aggregate_bytes": aggregate_bytes,
                "maximum_bytes": quota.max_aggregate_bytes,
            })));
        }
    }
    Ok(())
}

fn set_capture_pipe_nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    let descriptor = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn spawn_bounded_capture_reader<R>(
    name: &str,
    reader: R,
    limit: usize,
    stop: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<std::io::Result<BoundedPipeCapture>>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || drain_bounded_capture(reader, limit, &stop))
}

fn drain_bounded_capture(
    mut reader: impl Read,
    limit: usize,
    stop: &AtomicBool,
) -> std::io::Result<BoundedPipeCapture> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                let retained = size.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..retained]);
                truncated |= retained < size;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(BoundedPipeCapture { bytes, truncated })
}

fn finish_captured_readers(
    stdout_reader: thread::JoinHandle<std::io::Result<BoundedPipeCapture>>,
    stderr_reader: thread::JoinHandle<std::io::Result<BoundedPipeCapture>>,
    stop: Arc<AtomicBool>,
    process_group: Option<i32>,
) -> ServiceResult<(BoundedPipeCapture, BoundedPipeCapture, bool)> {
    let deadline = Instant::now()
        .checked_add(COMMAND_PIPE_DRAIN_TIMEOUT)
        .unwrap_or_else(Instant::now);
    while !(stdout_reader.is_finished() && stderr_reader.is_finished()) && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    let fully_drained = stdout_reader.is_finished() && stderr_reader.is_finished();
    if !fully_drained {
        if let Some(process_group) = process_group {
            unsafe {
                libc::kill(-process_group, libc::SIGKILL);
            }
        }
        stop.store(true, Ordering::Release);
    }
    let stdout = join_captured_reader(stdout_reader, "output");
    let stderr = join_captured_reader(stderr_reader, "diagnostics");
    let stdout = stdout?;
    let stderr = stderr?;
    Ok((stdout, stderr, fully_drained))
}

fn join_captured_reader(
    reader: thread::JoinHandle<std::io::Result<BoundedPipeCapture>>,
    label: &str,
) -> ServiceResult<BoundedPipeCapture> {
    reader
        .join()
        .map_err(|_| {
            VideoServiceError::new(
                "video.command_read_failed",
                format!("The local media command {label} reader failed"),
            )
        })?
        .map_err(|error| {
            VideoServiceError::io(
                "video.command_read_failed",
                "The local media command output could not be read",
                error,
            )
        })
}

fn stop_captured_process_group(child: &mut Child) {
    let process_group = i32::try_from(child.id()).ok();
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_nonblocking_chunks(
    reader: &mut impl Read,
    max_chunks: usize,
) -> std::io::Result<(Vec<Vec<u8>>, bool)> {
    let mut chunks = Vec::with_capacity(max_chunks.min(16));
    let mut buffer = [0_u8; 16 * 1024];
    while chunks.len() < max_chunks {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok((chunks, true)),
            Ok(size) => chunks.push(buffer[..size].to_vec()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    Ok((chunks, false))
}

fn kill_process_group_by_id(child_id: u32) {
    if let Ok(process_group) = i32::try_from(child_id) {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

fn command_failed_error(
    code: &'static str,
    message: &'static str,
    captured: &CapturedOutput,
    retryable: bool,
) -> VideoServiceError {
    VideoServiceError::new(code, message)
        .retryable(retryable)
        .details(json!({
            "exit_code": captured.status.code(),
            "diagnostic": truncate_chars(&String::from_utf8_lossy(&captured.stderr), 4_000),
        }))
}

#[allow(clippy::too_many_arguments)]
fn emit_progress(
    callback: Option<&ProgressCallback>,
    job_id: &str,
    project_id: &str,
    phase: &str,
    progress: f64,
    message: &str,
    playable_artifact: Option<Value>,
    metrics: Option<Value>,
) {
    let Some(callback) = callback else {
        return;
    };
    let event = VideoServiceProgress {
        job_id: job_id.to_string(),
        project_id: project_id.to_string(),
        phase: phase.to_string(),
        progress: progress.clamp(0.0, 1.0),
        message: message.to_string(),
        playable_artifact,
        metrics,
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(event)));
}

fn emit_job_state_error(
    callback: Option<&ProgressCallback>,
    job_id: &str,
    project_id: &str,
    attempted_state: &str,
    store_error: String,
) {
    let error = VideoServiceError::new(
        "video.job_state_failed",
        "The task finished locally but its durable terminal state could not be saved",
    )
    .retryable(true)
    .details(json!({
        "attempted_state": attempted_state,
        "store_error": store_error,
    }));
    emit_progress(
        callback,
        job_id,
        project_id,
        "failed",
        1.0,
        &error.message,
        None,
        Some(json!({ "error": error })),
    );
}

fn extend_bounded(target: &mut Vec<u8>, chunk: &[u8], maximum: usize) {
    if target.len() >= maximum {
        return;
    }
    let available = maximum - target.len();
    target.extend_from_slice(&chunk[..chunk.len().min(available)]);
}

fn copy_file_cancelable<F>(
    mut input: File,
    expected_identity: LocalSourceIdentity,
    destination: &Path,
    cancel: &AtomicBool,
    mut progress: F,
) -> ServiceResult<String>
where
    F: FnMut(f64),
{
    let opened_identity =
        LocalSourceIdentity::from_metadata(&input.metadata().map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected media could not be inspected",
                error,
            )
        })?);
    if opened_identity != expected_identity {
        return Err(VideoServiceError::new(
            "video.source_changed",
            "The selected local media changed after validation; choose it again",
        )
        .details(json!({
            "expected": format!("{}:{}:{}", expected_identity.device, expected_identity.inode, expected_identity.size),
            "actual": format!("{}:{}:{}", opened_identity.device, opened_identity.inode, opened_identity.size),
        })));
    }
    let total = expected_identity.size;
    validate_source_size(total)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(destination)
        .map_err(|error| {
            VideoServiceError::io(
                "video.copy_failed",
                "The managed source could not be staged",
                error,
            )
        })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut copied = 0_u64;
    let mut last_fraction = -1.0_f64;
    let result = (|| -> ServiceResult<String> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(VideoServiceError::cancelled());
            }
            let size = input.read(&mut buffer).map_err(|error| {
                VideoServiceError::io(
                    "video.copy_failed",
                    "The selected media could not be read",
                    error,
                )
            })?;
            if size == 0 {
                break;
            }
            output.write_all(&buffer[..size]).map_err(|error| {
                VideoServiceError::io(
                    "video.copy_failed",
                    "The managed source could not be written",
                    error,
                )
            })?;
            hasher.update(&buffer[..size]);
            copied = copied.saturating_add(size as u64);
            validate_source_size(copied)?;
            let fraction = if total == 0 {
                1.0
            } else {
                (copied as f64 / total as f64).clamp(0.0, 1.0)
            };
            if fraction - last_fraction >= 0.01 || fraction >= 1.0 {
                progress(fraction);
                last_fraction = fraction;
            }
        }
        let final_identity =
            LocalSourceIdentity::from_metadata(&input.metadata().map_err(|error| {
                VideoServiceError::io(
                    "video.source_not_found",
                    "The selected media could not be re-inspected after copying",
                    error,
                )
            })?);
        if copied != total || final_identity != expected_identity {
            return Err(VideoServiceError::new(
                "video.source_changed",
                "The selected local media changed while it was being copied; choose it again",
            ));
        }
        output.sync_all().map_err(|error| {
            VideoServiceError::io(
                "video.copy_failed",
                "The managed source could not be synchronized",
                error,
            )
        })?;
        secure_managed_file(destination)?;
        Ok(hex_digest(hasher.finalize().as_slice()))
    })();
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(destination);
    }
    result
}

fn sha256_file(path: &Path) -> ServiceResult<String> {
    sha256_file_with_cancel(path, None)
}

fn sha256_file_with_cancel(path: &Path, cancel: Option<&AtomicBool>) -> ServiceResult<String> {
    let mut file = File::open(path).map_err(|error| {
        VideoServiceError::io(
            "video.artifact_not_found",
            "The artifact could not be opened",
            error,
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let size = file.read(&mut buffer).map_err(|error| {
            VideoServiceError::io(
                "video.integrity_failed",
                "The artifact checksum could not be calculated",
                error,
            )
        })?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes).as_slice())
}

/// Stable semantic identity for a published output within one canonical
/// project version. The revision scope makes content-equivalent A -> B -> A
/// renders publishable on the later version (Store correctly forbids moving
/// an existing output ID between versions), while durable retries on the same
/// version still converge. Role and variation keep byte-identical alternatives
/// distinct.
fn stable_output_id(
    project_id: &str,
    version_revision: i64,
    role: &str,
    cache_key: &str,
    variation: u16,
) -> String {
    let identity = json!([project_id, version_revision, role, cache_key, variation]).to_string();
    format!("video-output-{}", sha256_bytes(identity.as_bytes()))
}

/// Durable import identity. A resumed child job must address the same source
/// and derived rows/files even when the process stopped after an asset upsert
/// but before the guarded manifest commit.
fn stable_import_asset_id(project_id: &str, job_id: &str, role: &str, key: &str) -> String {
    let identity = json!([project_id, job_id, role, key]).to_string();
    format!("video-asset-{}", sha256_bytes(identity.as_bytes()))
}

/// Locate a previously cached poster for `key`, whatever image format yt-dlp produced.
fn find_cached_thumbnail(directory: &Path, key: &str) -> Option<PathBuf> {
    THUMBNAIL_EXTENSIONS.iter().find_map(|extension| {
        let candidate = directory.join(format!("{key}.{extension}"));
        candidate
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() > 0)
            .map(|_| candidate)
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn typed_source_asset(source: &ManagedSource) -> ServiceResult<SourceAsset> {
    let video_stream = source.probe.primary_video_stream.and_then(|index| {
        source
            .probe
            .streams
            .iter()
            .find(|stream| stream.index == index)
    });
    let frame_rate = video_stream.and_then(|stream| {
        stream
            .average_frame_rate
            .as_deref()
            .and_then(parse_rational_frame_rate)
            .or_else(|| {
                stream
                    .real_frame_rate
                    .as_deref()
                    .and_then(parse_rational_frame_rate)
            })
    });
    let typed = SourceAsset {
        id: source.id.clone(),
        kind: source.kind.clone(),
        managed_path: source.relative_path.clone(),
        sha256: source.sha256.clone(),
        probe: super::MediaProbe {
            duration_us: Microseconds(source.probe.duration_us),
            width: video_stream.and_then(|stream| stream.width),
            height: video_stream.and_then(|stream| stream.height),
            frame_rate,
            has_video: source.probe.primary_video_stream.is_some(),
            has_audio: source.probe.primary_audio_stream.is_some(),
            format_name: source
                .probe
                .format_name
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "unknown".to_string()),
        },
        provenance: source.provenance.clone(),
        rights_confirmation_id: source.rights.as_ref().map(|rights| rights.id.clone()),
    };
    typed.validate()?;
    Ok(typed)
}

fn parse_rational_frame_rate(value: &str) -> Option<RationalFrameRate> {
    let (numerator, denominator) = value.trim().split_once('/')?;
    let mut numerator = numerator.parse::<u32>().ok()?;
    let mut denominator = denominator.parse::<u32>().ok()?;
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let divisor = gcd_u32(numerator, denominator);
    numerator /= divisor;
    denominator /= divisor;
    let rate = RationalFrameRate {
        numerator,
        denominator,
    };
    rate.validate().ok().map(|_| rate)
}

fn gcd_u32(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn media_cache_key(
    stage: CacheStage,
    source_sha256: &str,
    runtime: &MediaRuntimeStatus,
    profile: Value,
) -> ServiceResult<String> {
    Ok(CacheKeyBuilder::new(stage, SERVICE_VERSION)
        .artifact("source", source_sha256)
        .tool_version(
            "ffmpeg",
            runtime
                .ffmpeg
                .version
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        )
        .profile(profile)
        .build()?
        .into_string())
}

#[allow(clippy::too_many_arguments)]
fn product_from_media(
    video_root: &Path,
    path: PathBuf,
    cache_key: String,
    kind: &str,
    mime_type: &str,
    role: RenderArtifactRole,
    probe: RuntimeMediaProbe,
) -> ServiceResult<DerivedProduct> {
    ensure_path_in_root(video_root, &path)?;
    let video_stream = probe
        .primary_video_stream
        .and_then(|index| probe.streams.iter().find(|stream| stream.index == index));
    let sha256 = sha256_file(&path)?;
    Ok(DerivedProduct {
        id: new_id(),
        kind: kind.to_string(),
        path,
        sha256,
        cache_key,
        mime_type: mime_type.to_string(),
        role,
        duration_us: Some(probe.duration_us),
        width: video_stream.and_then(|stream| stream.width),
        height: video_stream.and_then(|stream| stream.height),
        probe: serde_json::to_value(probe).map_err(json_error)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn product_from_image(
    video_root: &Path,
    path: PathBuf,
    cache_key: String,
    kind: &str,
    mime_type: &str,
    role: RenderArtifactRole,
    dimensions: Option<(u32, u32)>,
) -> ServiceResult<DerivedProduct> {
    ensure_path_in_root(video_root, &path)?;
    validate_image_file(&path)?;
    let sha256 = sha256_file(&path)?;
    let (width, height) =
        dimensions.map_or((None, None), |(width, height)| (Some(width), Some(height)));
    Ok(DerivedProduct {
        id: new_id(),
        kind: kind.to_string(),
        path,
        sha256,
        cache_key,
        mime_type: mime_type.to_string(),
        role,
        duration_us: None,
        width,
        height,
        probe: json!({ "width": width, "height": height }),
    })
}

fn ensure_path_in_root(root: &Path, path: &Path) -> ServiceResult<()> {
    let root = fs::canonicalize(root).map_err(|error| {
        VideoServiceError::io(
            "video.storage_unavailable",
            "Managed video storage could not be resolved",
            error,
        )
    })?;
    let path = fs::canonicalize(path).map_err(|error| {
        VideoServiceError::io(
            "video.artifact_not_found",
            "The managed artifact could not be resolved",
            error,
        )
    })?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(VideoServiceError::new(
            "video.unsafe_artifact_path",
            "The artifact is outside managed video storage",
        ));
    }
    secure_managed_file(&path)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageInspection {
    mime_type: VisualMimeType,
    width: u32,
    height: u32,
    has_alpha: bool,
    size_bytes: u64,
}

fn validate_image_file(path: &Path) -> Result<(), MediaError> {
    inspect_image_file(path).map(|_| ())
}

fn inspect_image_file(path: &Path) -> Result<ImageInspection, MediaError> {
    let metadata = fs::metadata(path).map_err(|error| {
        MediaError::new("image_not_found", "The generated image could not be opened")
            .detail(error.to_string())
    })?;
    if !metadata.is_file() || metadata.len() < 16 || metadata.len() > MAX_VISUAL_ASSET_BYTES {
        return Err(MediaError::new(
            "invalid_image",
            "The image is empty or exceeds the supported 256 MiB limit",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            MediaError::new(
                "invalid_image",
                "The generated image could not be validated",
            )
            .detail(error.to_string())
        })?;
    inspect_image_reader(&mut file, metadata.len())
}

fn inspect_image_reader(file: &mut File, size_bytes: u64) -> Result<ImageInspection, MediaError> {
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        MediaError::new("invalid_image", "The image header could not be read")
            .detail(error.to_string())
    })?;
    let mut header = [0_u8; 30];
    file.read_exact(&mut header).map_err(|error| {
        MediaError::new("invalid_image", "The image header is incomplete").detail(error.to_string())
    })?;
    let (mime_type, width, height, has_alpha) =
        if header.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            if &header[12..16] != b"IHDR" {
                return Err(MediaError::new(
                    "invalid_image",
                    "The PNG image has no canonical IHDR header",
                ));
            }
            let width = u32::from_be_bytes(header[16..20].try_into().expect("PNG width"));
            let height = u32::from_be_bytes(header[20..24].try_into().expect("PNG height"));
            (
                VisualMimeType::Png,
                width,
                height,
                matches!(header[25], 4 | 6),
            )
        } else if header.starts_with(&[0xff, 0xd8, 0xff]) {
            file.seek(SeekFrom::Start(2)).map_err(|error| {
                MediaError::new("invalid_image", "The JPEG image could not be inspected")
                    .detail(error.to_string())
            })?;
            let (width, height) = inspect_jpeg_dimensions(file)?;
            (VisualMimeType::Jpeg, width, height, false)
        } else if header.starts_with(b"RIFF") && &header[8..12] == b"WEBP" {
            let (width, height, has_alpha) = inspect_webp_dimensions(&header)?;
            (VisualMimeType::Webp, width, height, has_alpha)
        } else {
            return Err(MediaError::new(
                "invalid_image",
                "The image is not PNG, JPEG, or WebP data",
            ));
        };
    if width == 0
        || height == 0
        || width > MAX_VISUAL_DIMENSION
        || height > MAX_VISUAL_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_VISUAL_PIXELS
    {
        return Err(MediaError::new(
            "invalid_image_dimensions",
            "The image dimensions exceed the supported visual envelope",
        ));
    }
    Ok(ImageInspection {
        mime_type,
        width,
        height,
        has_alpha,
        size_bytes,
    })
}

fn inspect_exact_visual_source(
    path: &Path,
) -> ServiceResult<(LocalSourceIdentity, ImageInspection, String)> {
    if !path.is_absolute() {
        return Err(VideoServiceError::new(
            "video.invalid_visual_source",
            "Visual source paths must be absolute",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            VideoServiceError::io(
                "video.visual_not_found",
                "The visual source could not be opened safely",
                error,
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.visual_not_found",
            "The visual source could not be inspected",
            error,
        )
    })?;
    if !metadata.is_file() || metadata.len() < 16 || metadata.len() > MAX_VISUAL_ASSET_BYTES {
        return Err(VideoServiceError::new(
            "video.invalid_visual",
            "The visual source is not a supported bounded regular file",
        ));
    }
    let identity = LocalSourceIdentity::from_metadata(&metadata);
    let inspection = inspect_image_reader(&mut file, metadata.len())?;
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        VideoServiceError::io(
            "video.visual_read_failed",
            "The visual source could not be prepared",
            error,
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let size = file.read(&mut buffer).map_err(|error| {
            VideoServiceError::io(
                "video.visual_read_failed",
                "The visual source could not be hashed",
                error,
            )
        })?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    let final_identity = LocalSourceIdentity::from_metadata(&file.metadata().map_err(|error| {
        VideoServiceError::io(
            "video.visual_not_found",
            "The visual source could not be re-inspected",
            error,
        )
    })?);
    if final_identity != identity {
        return Err(VideoServiceError::new(
            "video.visual_changed",
            "The visual source changed while its authorization was created",
        ));
    }
    Ok((
        identity,
        inspection,
        hex_digest(hasher.finalize().as_slice()),
    ))
}

fn inspect_jpeg_dimensions(file: &mut File) -> Result<(u32, u32), MediaError> {
    let mut inspected = 2_u64;
    while inspected < 16 * 1024 * 1024 {
        let mut prefix = [0_u8; 2];
        file.read_exact(&mut prefix).map_err(|error| {
            MediaError::new("invalid_image", "The JPEG marker stream is incomplete")
                .detail(error.to_string())
        })?;
        inspected += 2;
        if prefix[0] != 0xff {
            return Err(MediaError::new(
                "invalid_image",
                "The JPEG marker stream is invalid",
            ));
        }
        let marker = prefix[1];
        if matches!(marker, 0xd8 | 0xd9) || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let mut length_bytes = [0_u8; 2];
        file.read_exact(&mut length_bytes).map_err(|error| {
            MediaError::new("invalid_image", "The JPEG segment length is incomplete")
                .detail(error.to_string())
        })?;
        inspected += 2;
        let length = u16::from_be_bytes(length_bytes);
        if length < 2 {
            return Err(MediaError::new(
                "invalid_image",
                "The JPEG contains an invalid segment length",
            ));
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if length < 7 {
                return Err(MediaError::new(
                    "invalid_image",
                    "The JPEG frame header is incomplete",
                ));
            }
            let mut dimensions = [0_u8; 5];
            file.read_exact(&mut dimensions).map_err(|error| {
                MediaError::new("invalid_image", "The JPEG dimensions could not be read")
                    .detail(error.to_string())
            })?;
            return Ok((
                u32::from(u16::from_be_bytes([dimensions[3], dimensions[4]])),
                u32::from(u16::from_be_bytes([dimensions[1], dimensions[2]])),
            ));
        }
        let skip = i64::from(length) - 2;
        file.seek(SeekFrom::Current(skip)).map_err(|error| {
            MediaError::new("invalid_image", "The JPEG segment could not be inspected")
                .detail(error.to_string())
        })?;
        inspected = inspected.saturating_add(u64::from(length) - 2);
    }
    Err(MediaError::new(
        "invalid_image",
        "The JPEG frame header is missing or exceeds the inspection limit",
    ))
}

fn inspect_webp_dimensions(header: &[u8; 30]) -> Result<(u32, u32, bool), MediaError> {
    match &header[12..16] {
        b"VP8X" => {
            let width = 1
                + u32::from(header[24])
                + (u32::from(header[25]) << 8)
                + (u32::from(header[26]) << 16);
            let height = 1
                + u32::from(header[27])
                + (u32::from(header[28]) << 8)
                + (u32::from(header[29]) << 16);
            Ok((width, height, header[20] & 0x10 != 0))
        }
        b"VP8L" if header[20] == 0x2f => {
            let width = 1 + u32::from(header[21]) + ((u32::from(header[22]) & 0x3f) << 8);
            let height = 1
                + (u32::from(header[22]) >> 6)
                + (u32::from(header[23]) << 2)
                + ((u32::from(header[24]) & 0x0f) << 10);
            Ok((width, height, true))
        }
        b"VP8 " if header[23..26] == [0x9d, 0x01, 0x2a] => {
            let width = u32::from(u16::from_le_bytes([header[26], header[27]]) & 0x3fff);
            let height = u32::from(u16::from_le_bytes([header[28], header[29]]) & 0x3fff);
            Ok((width, height, false))
        }
        _ => Err(MediaError::new(
            "invalid_image",
            "The WebP image header is unsupported or incomplete",
        )),
    }
}

fn force_image_codec(plan: &mut RenderCommandPlan, codec: &str) {
    fn insert(command: &mut RenderCommand, codec: &str) {
        let output = command.args.pop();
        command.args.push(OsString::from("-c:v"));
        command.args.push(OsString::from(codec));
        if let Some(output) = output {
            command.args.push(output);
        }
    }
    insert(&mut plan.primary, codec);
    if let Some(fallback) = &mut plan.software_fallback {
        insert(fallback, codec);
    }
}

fn select_manifest_source<'a>(
    manifest: &'a VideoProjectManifest,
    requested_id: Option<&str>,
) -> ServiceResult<&'a SourceAsset> {
    if let Some(id) = requested_id {
        manifest
            .source_assets
            .iter()
            .find(|source| source.id == id)
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.source_not_found",
                    "The selected project source was not found",
                )
            })
    } else {
        manifest.source_assets.first().ok_or_else(|| {
            VideoServiceError::new(
                "video.source_required",
                "Import or generate a source before rendering video",
            )
        })
    }
}

/// Whether this episode already has something to look at that soundAr did not draw for it.
///
/// Imported video counts, and so does any still the user supplied or generated deliberately. Only
/// soundAr's own fallback card does not, because a card is what gets replaced once there is a real
/// picture to replace it with.
fn episode_has_own_picture(manifest: &VideoProjectManifest) -> bool {
    if manifest
        .source_assets
        .iter()
        .any(|source| source.probe.has_video)
    {
        return true;
    }
    manifest.visual_assets.iter().any(|asset| {
        !matches!(asset.provenance.kind, ProvenanceKind::GeneratedLocally)
            || !asset.id.starts_with("cover-")
    })
}

/// Whether a scene with no imported source is exactly accounted for by the dialogue performed
/// inside it.
///
/// The scene must begin where its first line begins and end where its last line ends, and every
/// line inside it must have a published take. Silence between lines is the performance's own
/// timing, not a hole in the scene, so beats are expected rather than treated as missing media.
fn scene_is_covered_by_its_turns(manifest: &VideoProjectManifest, scene: &ReviewedScene) -> bool {
    if scene.source_asset_id.is_some() || scene.source_range.is_some() {
        return false;
    }
    let scene_start = scene.timeline_start_us.0;
    let scene_end = scene_start + scene.timeline_duration_us.0;
    let mut first = i64::MAX;
    let mut last = i64::MIN;
    let mut any = false;
    for clip in manifest.tracks.iter().flat_map(|track| &track.clips) {
        if clip.turn_id.is_none() {
            continue;
        }
        let start = clip.timeline_start_us.0;
        let end = start + clip.timeline_duration_us.0;
        if start < scene_start || end > scene_end {
            // A line outside the scene means the scene does not describe the performance.
            return false;
        }
        // An unperformed line has nothing to render; it must not be counted as covering anything.
        if clip.media.render_artifact_id.is_none() {
            return false;
        }
        any = true;
        first = first.min(start);
        last = last.max(end);
    }
    any && first == scene_start && last == scene_end
}

fn validate_timeline_render_contract(
    manifest: &VideoProjectManifest,
    profile: TimelineRenderProfile,
) -> ServiceResult<()> {
    if manifest.reviewed_scenes.is_empty() {
        return Err(VideoServiceError::new(
            "video.reviewed_scenes_required",
            "Review at least one scene before rendering the timeline",
        ));
    }
    for scene in &manifest.reviewed_scenes {
        if matches!(scene.review_state, super::ReviewState::Rejected) {
            return Err(VideoServiceError::new(
                "video.rejected_scene",
                "Rejected scenes cannot be rendered",
            )
            .details(json!({ "scene_id": scene.id })));
        }
        if profile == TimelineRenderProfile::Final
            && !matches!(scene.review_state, super::ReviewState::Approved)
        {
            return Err(VideoServiceError::new(
                "video.final_review_required",
                "Every scene must be approved before final export",
            )
            .details(json!({ "scene_id": scene.id })));
        }
        let aligned = manifest
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .any(|clip| {
                clip.scene_id.as_deref() == Some(scene.id.as_str())
                    && clip.timeline_start_us == scene.timeline_start_us
                    && clip.timeline_duration_us == scene.timeline_duration_us
                    && match (scene.source_asset_id.as_deref(), scene.source_range) {
                        (Some(source_id), Some(source_range)) => {
                            clip.media.source_asset_id.as_deref() == Some(source_id)
                                && clip.source_range == source_range
                        }
                        (None, None) => clip.media.render_artifact_id.is_some(),
                        _ => false,
                    }
            })
            // A performed scene is backed by the lines performed in it rather than by one clip.
            // A conversation is many takes with beats between them, so requiring a single clip
            // spanning the scene would mean no spoken scene could ever be rendered.
            || scene_is_covered_by_its_turns(manifest, scene);
        if !aligned {
            return Err(VideoServiceError::new(
                "video.timeline_scene_track_mismatch",
                "A reviewed scene is not aligned with its canonical timeline clip",
            )
            .details(json!({ "scene_id": scene.id })));
        }
    }
    if manifest
        .audio_mix
        .tracks
        .iter()
        .any(|track| track.ducking.is_some())
    {
        return Err(VideoServiceError::new(
            "video.timeline_feature_unsupported",
            "Sidechain ducking needs a compatible render plan",
        ));
    }
    Ok(())
}

fn render_resource_request(profile: RenderProfile, nvenc: bool) -> ResourceRequest {
    match (profile, nvenc) {
        (RenderProfile::Final, true) => ResourceRequest {
            class: ResourceClass::Heavy,
            vram_mb: 2_048,
            cpu_threads: 8,
            io_slots: 3,
            nvenc_sessions: 1,
        },
        (RenderProfile::Final, false) => ResourceRequest {
            class: ResourceClass::Heavy,
            vram_mb: 0,
            cpu_threads: 16,
            io_slots: 3,
            nvenc_sessions: 0,
        },
        (_, true) => ResourceRequest::medium_nvenc(),
        (_, false) => ResourceRequest {
            class: ResourceClass::Medium,
            vram_mb: 0,
            cpu_threads: 8,
            io_slots: 2,
            nvenc_sessions: 0,
        },
    }
}

fn resource_class_name(class: ResourceClass) -> &'static str {
    match class {
        ResourceClass::Light => "light",
        ResourceClass::Medium => "medium",
        ResourceClass::Heavy => "heavy",
        ResourceClass::Exclusive => "exclusive",
    }
}

fn portrait_dimensions(profile: RenderProfile) -> (u32, u32) {
    match profile {
        RenderProfile::Proxy => (540, 960),
        RenderProfile::Preview => (720, 1280),
        RenderProfile::Final => (1080, 1920),
    }
}

fn service_encoder_arguments(encoder: VideoEncoder, profile: RenderProfile) -> Vec<OsString> {
    match encoder {
        VideoEncoder::H264Nvenc => vec![
            OsString::from("-c:v"),
            OsString::from("h264_nvenc"),
            OsString::from("-preset"),
            OsString::from(if profile == RenderProfile::Final {
                "p5"
            } else {
                "p3"
            }),
            OsString::from("-tune"),
            OsString::from("hq"),
            OsString::from("-rc"),
            OsString::from("vbr"),
            OsString::from("-cq"),
            OsString::from(if profile == RenderProfile::Final {
                "19"
            } else {
                "24"
            }),
            OsString::from("-b:v"),
            OsString::from("0"),
            OsString::from("-pix_fmt"),
            OsString::from("yuv420p"),
        ],
        VideoEncoder::Libx264 => vec![
            OsString::from("-c:v"),
            OsString::from("libx264"),
            OsString::from("-preset"),
            OsString::from(if profile == RenderProfile::Final {
                "medium"
            } else {
                "veryfast"
            }),
            OsString::from("-crf"),
            OsString::from(if profile == RenderProfile::Final {
                "19"
            } else {
                "24"
            }),
            OsString::from("-pix_fmt"),
            OsString::from("yuv420p"),
        ],
        VideoEncoder::Image | VideoEncoder::AudioOnly => Vec::new(),
    }
}

fn probe_duration_seconds(path: &Path, runtime: &MediaRuntimeStatus) -> ServiceResult<f64> {
    let ffprobe = required_tool_path(
        &runtime.ffprobe,
        "video.ffprobe_unavailable",
        "FFprobe is required to inspect source duration",
    )?;
    let duration_us = probe_media(path, ffprobe)?.duration_us;
    Ok(duration_us as f64 / 1_000_000.0)
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

fn required_tool_path<'a>(
    tool: &'a super::MediaToolStatus,
    code: &'static str,
    message: &'static str,
) -> ServiceResult<&'a Path> {
    if !tool.available {
        return Err(VideoServiceError::new(code, message).details(json!({
            "setup_action": tool.setup_action,
            "diagnostic": tool.diagnostic,
        })));
    }
    tool.path.as_deref().ok_or_else(|| {
        VideoServiceError::new(code, message).details(json!({
            "setup_action": tool.setup_action,
            "diagnostic": "Tool discovery reported availability without an executable path",
        }))
    })
}

fn safe_extension(path: &Path, has_video: bool) -> String {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|value| {
            (1..=10).contains(&value.len())
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    extension.unwrap_or_else(|| if has_video { "mp4" } else { "m4a" }.to_string())
}

fn display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| truncate_chars(value, 160))
        .unwrap_or_else(|| "Imported media".to_string())
}

fn media_mime(path: &Path, has_video: bool) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        _ if has_video => "video/*",
        _ => "audio/*",
    }
}

/// Builds the one-source yt-dlp invocation used after exact URL validation and
/// rights confirmation. `--max-downloads 1` must not be added here: yt-dlp
/// reports its intentional limit with exit 101 even after the first download
/// succeeds. Single-source behavior is instead enforced without a success
/// sentinel by the exact positional URL, `--no-playlist`, the explicit first
/// item selector, the unique output prefix/quota, and
/// `single_downloaded_file` after a zero exit status.
fn yt_dlp_single_source_download_args(
    output_template: &Path,
    canonical_url: &str,
    proxy_url: &str,
) -> Vec<OsString> {
    vec![
        OsString::from("--ignore-config"),
        OsString::from("--no-playlist"),
        OsString::from("--playlist-items"),
        OsString::from("1"),
        OsString::from("--max-filesize"),
        OsString::from("8G"),
        OsString::from("--match-filter"),
        OsString::from("!is_live & duration <= 21600"),
        OsString::from("--socket-timeout"),
        OsString::from("20"),
        OsString::from("--retries"),
        OsString::from("3"),
        OsString::from("--fragment-retries"),
        OsString::from("3"),
        OsString::from("--no-progress"),
        OsString::from("--no-warnings"),
        OsString::from("--restrict-filenames"),
        OsString::from("--merge-output-format"),
        OsString::from("mp4"),
        OsString::from("--format"),
        OsString::from("bv*+ba/b"),
        OsString::from("--proxy"),
        OsString::from(proxy_url),
        OsString::from("--output"),
        output_template.as_os_str().to_os_string(),
        OsString::from("--"),
        OsString::from(canonical_url),
    ]
}

fn single_downloaded_file(directory: &Path, prefix: &str) -> ServiceResult<PathBuf> {
    let mut candidates = fs::read_dir(directory)
        .map_err(|error| {
            VideoServiceError::io(
                "video.link_import_failed",
                "The downloaded source directory could not be read",
                error,
            )
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let metadata = entry.metadata().ok()?;
            (name.starts_with(prefix)
                && metadata.is_file()
                && !name.ends_with(".part")
                && !name.ends_with(".ytdl"))
            .then_some(entry.path())
        })
        .collect::<Vec<_>>();
    candidates.sort();
    if candidates.len() != 1 {
        return Err(VideoServiceError::new(
            "video.single_source_required",
            "Link import must produce exactly one media source",
        )
        .details(json!({ "candidate_count": candidates.len() })));
    }
    Ok(candidates.remove(0))
}

fn cleanup_prefixed_files(directory: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let matches = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix));
        if matches && entry.metadata().is_ok_and(|metadata| metadata.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn cleanup_failed_download(directory: &Path, prefix: &str, published_path: Option<&Path>) {
    cleanup_prefixed_files(directory, prefix);
    if let Some(path) = published_path {
        let is_owned_final = path.parent() == Some(directory)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("source-"));
        if is_owned_final {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopiedFileVisibility {
    ManagedPrivate,
    UserShareable,
}

fn copy_file_verified(
    source: &Path,
    destination: &Path,
    visibility: CopiedFileVisibility,
    cancel: &AtomicBool,
) -> ServiceResult<()> {
    // Never hard-link across a publication boundary. A package or user export
    // is writable independently and must not alias the managed render cache.
    let expected_sha256 = sha256_file_with_cancel(source, Some(cancel))?;
    let source_size = fs::metadata(source)
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package artifact could not be inspected",
                error,
            )
        })?
        .len();
    let mut input = File::open(source).map_err(|error| {
        VideoServiceError::io(
            "video.package_failed",
            "A package artifact could not be opened",
            error,
        )
    })?;
    let mode = match visibility {
        CopiedFileVisibility::ManagedPrivate => PRIVATE_FILE_MODE,
        CopiedFileVisibility::UserShareable => SHAREABLE_FILE_MODE,
    };
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(destination)
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package artifact could not be staged",
                error,
            )
        })?;
    let result = (|| -> ServiceResult<()> {
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(VideoServiceError::cancelled());
            }
            let size = input.read(&mut buffer).map_err(|error| {
                VideoServiceError::io(
                    "video.package_failed",
                    "A package artifact could not be read",
                    error,
                )
            })?;
            if size == 0 {
                break;
            }
            output.write_all(&buffer[..size]).map_err(|error| {
                VideoServiceError::io(
                    "video.package_failed",
                    "A package artifact could not be copied",
                    error,
                )
            })?;
            copied = copied.saturating_add(size as u64);
        }
        if copied != source_size {
            return Err(VideoServiceError::new(
                "video.package_failed",
                "A package artifact was copied incompletely",
            ));
        }
        output.sync_all().map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package artifact could not be synchronized",
                error,
            )
        })?;
        match visibility {
            CopiedFileVisibility::ManagedPrivate => secure_managed_file(destination)?,
            CopiedFileVisibility::UserShareable => output
                .set_permissions(fs::Permissions::from_mode(SHAREABLE_FILE_MODE))
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.export_failed",
                        "Shareable permissions could not be applied to an exported artifact",
                        error,
                    )
                })?,
        }
        let actual_sha256 = sha256_file_with_cancel(destination, Some(cancel))?;
        if actual_sha256 != expected_sha256 {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "A copied package artifact failed checksum verification",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(destination);
        return result;
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> ServiceResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package file could not be created",
                error,
            )
        })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package file could not be written",
                error,
            )
        })?;
    secure_managed_file(path)
}

#[cfg(test)]
fn valid_package_directory(path: &Path) -> bool {
    validate_package_directory(path).is_ok()
}

#[cfg(test)]
fn validate_package_directory(path: &Path) -> ServiceResult<()> {
    validate_package_directory_with_cancel(path, None)
}

fn package_regular_files_within_limit(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<Vec<(String, PathBuf, u64)>> {
    let directory_metadata = fs::symlink_metadata(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The publish package could not be inspected",
            error,
        )
    })?;
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The publish package is not a regular directory",
        ));
    }
    let mut aggregate_bytes = 0_u64;
    let mut files = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The package directory could not be read",
            error,
        )
    })? {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let entry = entry.map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "A package entry could not be read",
                error,
            )
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "A package entry could not be inspected",
                error,
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.package_invalid",
                "Publish packages may only contain regular files",
            ));
        }
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.package_invalid",
                    "A package filename is not valid UTF-8",
                )
            })?;
        if name.as_bytes().len() > u16::MAX as usize {
            return Err(VideoServiceError::new(
                "video.package_invalid",
                "A package filename is too long for ZIP publication",
            ));
        }
        aggregate_bytes = aggregate_bytes.saturating_add(metadata.len());
        // This metadata-only first pass prevents an oversized sparse or real
        // file from consuming minutes of checksum work before rejection.
        validate_package_aggregate_bytes(aggregate_bytes)?;
        files.push((name, entry_path, metadata.len()));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn validate_package_directory_with_cancel(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The publish package could not be inspected",
            error,
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The publish package is not a regular directory",
        ));
    }
    package_regular_files_within_limit(path, cancel)?;
    let manifest_path = path.join("package-manifest.json");
    let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The package manifest is missing",
            error,
        )
    })?;
    if !manifest_metadata.is_file()
        || manifest_metadata.file_type().is_symlink()
        || manifest_metadata.len() > MAX_CAPTURE_BYTES as u64
    {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The package manifest is not a bounded regular file",
        ));
    }
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The package manifest could not be read",
            error,
        )
    })?)
    .map_err(|error| {
        VideoServiceError::new(
            "video.package_invalid",
            "The package manifest is invalid JSON",
        )
        .details(json!({ "diagnostic": error.to_string() }))
    })?;
    if manifest.get("kind").and_then(Value::as_str) != Some("soundar_publish_package") {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The directory is not a soundAr publish package",
        ));
    }
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            VideoServiceError::new(
                "video.package_invalid",
                "The package manifest has no checksum inventory",
            )
        })?;
    let allowed = BTreeSet::from([
        "master.mp4",
        "timeline-manifest.json",
        "captions.srt",
        "README.txt",
    ]);
    let required = BTreeSet::from([
        "master.mp4".to_string(),
        "timeline-manifest.json".to_string(),
        "README.txt".to_string(),
    ]);
    let mut listed = BTreeSet::new();
    for record in files {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let name = value_string(record, "path")?;
        if !allowed.contains(name.as_str()) || !listed.insert(name.clone()) {
            return Err(VideoServiceError::new(
                "video.package_invalid",
                "The package checksum inventory contains an invalid or duplicate path",
            ));
        }
        let file_path = path.join(&name);
        let file_metadata = fs::symlink_metadata(&file_path).map_err(|error| {
            VideoServiceError::io("video.package_invalid", "A package file is missing", error)
        })?;
        if !file_metadata.is_file() || file_metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.package_invalid",
                "Publish packages may only contain regular files",
            ));
        }
        let expected_size = record
            .get("size_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_store_shape("files.size_bytes"))?;
        let expected_sha256 = value_string(record, "sha256")?;
        if file_metadata.len() != expected_size
            || sha256_file_with_cancel(&file_path, cancel)? != expected_sha256
        {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "A publish package file does not match its checksum inventory",
            )
            .details(json!({ "path": name })));
        }
    }
    if !required.is_subset(&listed) {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The publish package is missing required files",
        ));
    }
    let master_sha = sha256_file_with_cancel(&path.join("master.mp4"), cancel)?;
    if manifest
        .get("master")
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str)
        != Some(master_sha.as_str())
    {
        return Err(VideoServiceError::new(
            "video.integrity_failed",
            "The package master does not match its declared checksum",
        ));
    }
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The package directory could not be read",
            error,
        )
    })? {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let entry = entry.map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "A package entry could not be read",
                error,
            )
        })?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.package_invalid",
                    "A package entry has an invalid filename",
                )
            })?;
        actual.insert(name);
    }
    listed.insert("package-manifest.json".to_string());
    if actual != listed {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The publish package contains untracked files",
        ));
    }
    Ok(())
}

fn validate_package_identity(
    path: &Path,
    expected_cache_key: &str,
    expected_master_sha256: &str,
    expected_version_id: &str,
    expected_version_sha256: &str,
    expected_master_output_id: &str,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<()> {
    validate_package_directory_with_cancel(path, cancel)?;
    let manifest: Value = serde_json::from_slice(
        &fs::read(path.join("package-manifest.json")).map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "The package manifest could not be read",
                error,
            )
        })?,
    )
    .map_err(|error| {
        VideoServiceError::new(
            "video.package_invalid",
            "The package manifest is invalid JSON",
        )
        .details(json!({ "diagnostic": error.to_string() }))
    })?;
    let cache_key = manifest.get("cache_key").and_then(Value::as_str);
    let master_sha256 = manifest
        .get("master")
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str);
    let master_output_id = manifest
        .get("master")
        .and_then(|value| value.get("output_id"))
        .and_then(Value::as_str);
    let package_version = manifest
        .get("project")
        .and_then(|value| value.get("version"));
    let version_id = package_version
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str);
    let version_sha256 = package_version
        .and_then(|value| value.get("sha256"))
        .and_then(Value::as_str);
    if cache_key != Some(expected_cache_key)
        || master_sha256 != Some(expected_master_sha256)
        || master_output_id != Some(expected_master_output_id)
        || version_id != Some(expected_version_id)
        || version_sha256 != Some(expected_version_sha256)
    {
        return Err(VideoServiceError::new(
            "video.package_identity_mismatch",
            "An existing package does not belong to this exact project version and master output",
        )
        .details(json!({
            "expected_cache_key": expected_cache_key,
            "expected_version_id": expected_version_id,
            "expected_version_sha256": expected_version_sha256,
            "expected_master_output_id": expected_master_output_id,
        })));
    }
    Ok(())
}

const ZIP_LOCAL_FILE_HEADER: u32 = 0x0403_4b50;
const ZIP_CENTRAL_DIRECTORY_HEADER: u32 = 0x0201_4b50;
const ZIP_END_OF_CENTRAL_DIRECTORY: u32 = 0x0605_4b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY: u32 = 0x0606_4b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_EXTRA_FIELD: u16 = 0x0001;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PackageMemberIntegrity {
    size: u64,
    sha256: String,
    crc32: u32,
}

#[derive(Clone, Debug)]
struct ZipWriteMember {
    name: String,
    path: PathBuf,
    integrity: PackageMemberIntegrity,
    local_header_offset: u64,
}

fn package_member_inventory(
    package_directory: &Path,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<BTreeMap<String, PackageMemberIntegrity>> {
    let entries = package_regular_files_within_limit(package_directory, cancel)?;
    validate_package_directory_with_cancel(package_directory, cancel)?;
    let mut inventory = BTreeMap::new();
    for (name, path, expected_size) in entries {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "A package entry could not be inspected",
                error,
            )
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != expected_size
        {
            return Err(VideoServiceError::new(
                "video.package_changed",
                "A package file changed while its archive was being prepared",
            ));
        }
        let (crc32, size) = crc32_file(&path, cancel)?;
        if size != expected_size {
            return Err(VideoServiceError::new(
                "video.package_changed",
                "A package file changed while its archive was being prepared",
            )
            .retryable(true));
        }
        inventory.insert(
            name,
            PackageMemberIntegrity {
                size,
                sha256: sha256_file_with_cancel(&path, cancel)?,
                crc32,
            },
        );
    }
    Ok(inventory)
}

/// Publishes a deterministic, uncompressed ZIP beside the managed package
/// directory. Stored entries avoid any dependency on an optional system ZIP
/// executable and make member checksums independently verifiable. ZIP64 is
/// emitted when a long final master crosses the classic 4 GiB boundary.
fn write_publish_zip_atomic(
    package_directory: &Path,
    archive_path: &Path,
    cancel: &AtomicBool,
) -> ServiceResult<()> {
    if archive_path.exists() {
        let archive_metadata = fs::symlink_metadata(archive_path).map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "The existing publish ZIP could not be inspected",
                error,
            )
        })?;
        if !archive_metadata.is_file() || archive_metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.package_invalid",
                "The existing publish ZIP is not a regular file",
            ));
        }
        if archive_metadata.len() > MAX_PACKAGE_ARCHIVE_BYTES {
            return Err(VideoServiceError::new(
                "video.package_too_large",
                "The existing publish ZIP exceeds the bounded archive limit",
            )
            .details(json!({
                "archive_bytes": archive_metadata.len(),
                "maximum_bytes": MAX_PACKAGE_ARCHIVE_BYTES,
            })));
        }
    }
    let inventory = package_member_inventory(package_directory, Some(cancel))?;
    if archive_path.exists() {
        secure_managed_file(archive_path)?;
        validate_publish_zip_with_inventory(archive_path, &inventory, Some(cancel))?;
        return Ok(());
    }
    let parent = archive_path.parent().ok_or_else(|| {
        VideoServiceError::new(
            "video.package_failed",
            "The managed ZIP path has no parent directory",
        )
    })?;
    let archive_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("publish.zip");
    let staging = parent.join(format!(".{archive_name}.{}.partial", new_id()));
    let staging_result = (|| -> ServiceResult<()> {
        write_publish_zip_file(package_directory, &inventory, &staging, cancel)?;
        validate_publish_zip_with_inventory(&staging, &inventory, Some(cancel))?;
        secure_managed_file(&staging)
    })();
    if let Err(error) = staging_result {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    fs::rename(&staging, archive_path).map_err(|error| {
        let _ = fs::remove_file(&staging);
        VideoServiceError::io(
            "video.package_publish_failed",
            "The publish ZIP could not be atomically published",
            error,
        )
    })?;
    sync_directory(parent)?;
    secure_managed_file(archive_path)?;
    validate_publish_zip_with_inventory(archive_path, &inventory, Some(cancel))
}

fn write_publish_zip_file(
    package_directory: &Path,
    inventory: &BTreeMap<String, PackageMemberIntegrity>,
    destination: &Path,
    cancel: &AtomicBool,
) -> ServiceResult<()> {
    let mut archive = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(destination)
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "The publish ZIP staging file could not be created",
                error,
            )
        })?;
    let mut members = Vec::with_capacity(inventory.len());
    for (name, integrity) in inventory {
        if cancel.load(Ordering::Acquire) {
            return Err(VideoServiceError::cancelled());
        }
        let local_header_offset = archive.stream_position().map_err(zip_write_error)?;
        let needs_zip64 = integrity.size >= u32::MAX as u64;
        let mut extra = Vec::new();
        if needs_zip64 {
            push_zip_u16(&mut extra, ZIP64_EXTRA_FIELD);
            push_zip_u16(&mut extra, 16);
            push_zip_u64(&mut extra, integrity.size);
            push_zip_u64(&mut extra, integrity.size);
        }
        write_zip_u32(&mut archive, ZIP_LOCAL_FILE_HEADER)?;
        write_zip_u16(&mut archive, if needs_zip64 { 45 } else { 20 })?;
        write_zip_u16(&mut archive, 0)?; // flags
        write_zip_u16(&mut archive, 0)?; // stored, never recompressed
        write_zip_u16(&mut archive, 0)?; // deterministic DOS time
        write_zip_u16(&mut archive, 0x0021)?; // 1980-01-01
        write_zip_u32(&mut archive, integrity.crc32)?;
        let legacy_size = if needs_zip64 {
            u32::MAX
        } else {
            integrity.size as u32
        };
        write_zip_u32(&mut archive, legacy_size)?;
        write_zip_u32(&mut archive, legacy_size)?;
        write_zip_u16(&mut archive, name.len() as u16)?;
        write_zip_u16(&mut archive, extra.len() as u16)?;
        archive
            .write_all(name.as_bytes())
            .map_err(zip_write_error)?;
        archive.write_all(&extra).map_err(zip_write_error)?;
        let mut source = File::open(package_directory.join(name)).map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package member could not be opened for ZIP publication",
                error,
            )
        })?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(VideoServiceError::cancelled());
            }
            let size = source.read(&mut buffer).map_err(zip_write_error)?;
            if size == 0 {
                break;
            }
            archive
                .write_all(&buffer[..size])
                .map_err(zip_write_error)?;
            copied = copied.saturating_add(size as u64);
        }
        if copied != integrity.size {
            return Err(VideoServiceError::new(
                "video.package_changed",
                "A package file changed while the ZIP was being written",
            )
            .retryable(true));
        }
        members.push(ZipWriteMember {
            name: name.clone(),
            path: package_directory.join(name),
            integrity: integrity.clone(),
            local_header_offset,
        });
    }

    let central_offset = archive.stream_position().map_err(zip_write_error)?;
    let mut member_uses_zip64 = false;
    for member in &members {
        if cancel.load(Ordering::Acquire) {
            return Err(VideoServiceError::cancelled());
        }
        // Recheck the source after copying so concurrent mutation cannot produce
        // an archive whose bytes disagree with the managed directory.
        if fs::metadata(&member.path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.package_changed",
                    "A package member could not be re-inspected",
                    error,
                )
            })?
            .len()
            != member.integrity.size
            || sha256_file_with_cancel(&member.path, Some(cancel))? != member.integrity.sha256
        {
            return Err(VideoServiceError::new(
                "video.package_changed",
                "A package file changed while the ZIP was being written",
            )
            .retryable(true));
        }
        let zip64_size = member.integrity.size >= u32::MAX as u64;
        let zip64_offset = member.local_header_offset >= u32::MAX as u64;
        let needs_zip64 = zip64_size || zip64_offset;
        member_uses_zip64 |= needs_zip64;
        let mut extra = Vec::new();
        if needs_zip64 {
            let field_size = (if zip64_size { 16 } else { 0 }) + if zip64_offset { 8 } else { 0 };
            push_zip_u16(&mut extra, ZIP64_EXTRA_FIELD);
            push_zip_u16(&mut extra, field_size);
            if zip64_size {
                push_zip_u64(&mut extra, member.integrity.size);
                push_zip_u64(&mut extra, member.integrity.size);
            }
            if zip64_offset {
                push_zip_u64(&mut extra, member.local_header_offset);
            }
        }
        write_zip_u32(&mut archive, ZIP_CENTRAL_DIRECTORY_HEADER)?;
        write_zip_u16(&mut archive, if needs_zip64 { 0x032d } else { 0x0314 })?;
        write_zip_u16(&mut archive, if needs_zip64 { 45 } else { 20 })?;
        write_zip_u16(&mut archive, 0)?;
        write_zip_u16(&mut archive, 0)?;
        write_zip_u16(&mut archive, 0)?;
        write_zip_u16(&mut archive, 0x0021)?;
        write_zip_u32(&mut archive, member.integrity.crc32)?;
        let legacy_size = if zip64_size {
            u32::MAX
        } else {
            member.integrity.size as u32
        };
        write_zip_u32(&mut archive, legacy_size)?;
        write_zip_u32(&mut archive, legacy_size)?;
        write_zip_u16(&mut archive, member.name.len() as u16)?;
        write_zip_u16(&mut archive, extra.len() as u16)?;
        write_zip_u16(&mut archive, 0)?; // comment length
        write_zip_u16(&mut archive, 0)?; // disk number
        write_zip_u16(&mut archive, 0)?; // internal attributes
        write_zip_u32(&mut archive, 0o100644_u32 << 16)?;
        write_zip_u32(
            &mut archive,
            if zip64_offset {
                u32::MAX
            } else {
                member.local_header_offset as u32
            },
        )?;
        archive
            .write_all(member.name.as_bytes())
            .map_err(zip_write_error)?;
        archive.write_all(&extra).map_err(zip_write_error)?;
    }
    let central_end = archive.stream_position().map_err(zip_write_error)?;
    let central_size = central_end - central_offset;
    let needs_zip64_directory = member_uses_zip64
        || members.len() >= u16::MAX as usize
        || central_size >= u32::MAX as u64
        || central_offset >= u32::MAX as u64;
    if needs_zip64_directory {
        let zip64_offset = archive.stream_position().map_err(zip_write_error)?;
        write_zip_u32(&mut archive, ZIP64_END_OF_CENTRAL_DIRECTORY)?;
        write_zip_u64(&mut archive, 44)?;
        write_zip_u16(&mut archive, 45)?;
        write_zip_u16(&mut archive, 45)?;
        write_zip_u32(&mut archive, 0)?;
        write_zip_u32(&mut archive, 0)?;
        write_zip_u64(&mut archive, members.len() as u64)?;
        write_zip_u64(&mut archive, members.len() as u64)?;
        write_zip_u64(&mut archive, central_size)?;
        write_zip_u64(&mut archive, central_offset)?;
        write_zip_u32(&mut archive, ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR)?;
        write_zip_u32(&mut archive, 0)?;
        write_zip_u64(&mut archive, zip64_offset)?;
        write_zip_u32(&mut archive, 1)?;
    }
    write_zip_u32(&mut archive, ZIP_END_OF_CENTRAL_DIRECTORY)?;
    write_zip_u16(&mut archive, 0)?;
    write_zip_u16(&mut archive, 0)?;
    let legacy_count = if needs_zip64_directory {
        u16::MAX
    } else {
        members.len() as u16
    };
    write_zip_u16(&mut archive, legacy_count)?;
    write_zip_u16(&mut archive, legacy_count)?;
    write_zip_u32(
        &mut archive,
        if needs_zip64_directory {
            u32::MAX
        } else {
            central_size as u32
        },
    )?;
    write_zip_u32(
        &mut archive,
        if needs_zip64_directory {
            u32::MAX
        } else {
            central_offset as u32
        },
    )?;
    write_zip_u16(&mut archive, 0)?;
    archive.sync_all().map_err(zip_write_error)
}

#[cfg(test)]
fn validate_publish_zip(archive_path: &Path, package_directory: &Path) -> ServiceResult<()> {
    validate_publish_zip_with_cancel(archive_path, package_directory, None)
}

fn validate_publish_zip_with_cancel(
    archive_path: &Path,
    package_directory: &Path,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<()> {
    let inventory = package_member_inventory(package_directory, cancel)?;
    validate_publish_zip_with_inventory(archive_path, &inventory, cancel)
}

/// Reads every stored member and verifies CRC32, SHA-256, size, local-header
/// offset, and central-directory identity. This catches both truncated archives
/// and the subtler case where a writable export was modified in place.
fn validate_publish_zip_with_inventory(
    archive_path: &Path,
    expected: &BTreeMap<String, PackageMemberIntegrity>,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<()> {
    let metadata = fs::symlink_metadata(archive_path).map_err(|error| {
        VideoServiceError::io(
            "video.package_invalid",
            "The publish ZIP could not be inspected",
            error,
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(VideoServiceError::new(
            "video.package_invalid",
            "The publish ZIP is not a regular file",
        ));
    }
    let archive_len = metadata.len();
    if archive_len > MAX_PACKAGE_ARCHIVE_BYTES {
        return Err(VideoServiceError::new(
            "video.package_too_large",
            "The publish ZIP exceeds the bounded package output limit",
        )
        .details(json!({
            "archive_bytes": archive_len,
            "maximum_bytes": MAX_PACKAGE_ARCHIVE_BYTES,
        })));
    }
    let mut archive = File::open(archive_path).map_err(zip_read_error)?;
    let mut local_members = BTreeMap::<String, (PackageMemberIntegrity, u64)>::new();
    let central_offset = loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let header_offset = archive.stream_position().map_err(zip_read_error)?;
        let signature = read_zip_u32(&mut archive)?;
        if signature == ZIP_CENTRAL_DIRECTORY_HEADER {
            archive
                .seek(SeekFrom::Start(header_offset))
                .map_err(zip_read_error)?;
            break header_offset;
        }
        if signature != ZIP_LOCAL_FILE_HEADER {
            return Err(invalid_zip("The ZIP has an invalid local-file signature"));
        }
        let version_needed = read_zip_u16(&mut archive)?;
        let flags = read_zip_u16(&mut archive)?;
        let method = read_zip_u16(&mut archive)?;
        let dos_time = read_zip_u16(&mut archive)?;
        let dos_date = read_zip_u16(&mut archive)?;
        let crc32 = read_zip_u32(&mut archive)?;
        let compressed_legacy = read_zip_u32(&mut archive)?;
        let uncompressed_legacy = read_zip_u32(&mut archive)?;
        let name_len = read_zip_u16(&mut archive)? as usize;
        let extra_len = read_zip_u16(&mut archive)? as usize;
        if !matches!(version_needed, 20 | 45)
            || flags != 0
            || method != 0
            || dos_time != 0
            || dos_date != 0x0021
            || name_len == 0
        {
            return Err(invalid_zip(
                "The ZIP uses an unsupported or non-deterministic entry format",
            ));
        }
        let name = read_zip_name(&mut archive, name_len)?;
        let extra = read_zip_bytes(&mut archive, extra_len)?;
        let needs_uncompressed = uncompressed_legacy == u32::MAX;
        let needs_compressed = compressed_legacy == u32::MAX;
        let (zip64_uncompressed, zip64_compressed, _) =
            parse_zip64_extra(&extra, needs_uncompressed, needs_compressed, false)?;
        let uncompressed = if needs_uncompressed {
            zip64_uncompressed.ok_or_else(|| invalid_zip("The ZIP64 size is missing"))?
        } else {
            uncompressed_legacy as u64
        };
        let compressed = if needs_compressed {
            zip64_compressed.ok_or_else(|| invalid_zip("The ZIP64 size is missing"))?
        } else {
            compressed_legacy as u64
        };
        if compressed != uncompressed
            || archive
                .stream_position()
                .map_err(zip_read_error)?
                .saturating_add(compressed)
                > archive_len
        {
            return Err(invalid_zip("The ZIP entry has an invalid stored size"));
        }
        let mut remaining = compressed;
        let mut crc_state = 0xffff_ffff_u32;
        let mut sha = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err(VideoServiceError::cancelled());
            }
            let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            archive
                .read_exact(&mut buffer[..count])
                .map_err(zip_read_error)?;
            crc_state = crc32_update(crc_state, &buffer[..count]);
            sha.update(&buffer[..count]);
            remaining -= count as u64;
        }
        let integrity = PackageMemberIntegrity {
            size: uncompressed,
            sha256: format!("{:x}", sha.finalize()),
            crc32: !crc_state,
        };
        if integrity.crc32 != crc32
            || expected.get(&name) != Some(&integrity)
            || local_members
                .insert(name, (integrity, header_offset))
                .is_some()
        {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "A publish ZIP member does not match its checksum inventory",
            ));
        }
    };
    if local_members.len() != expected.len() {
        return Err(invalid_zip(
            "The ZIP does not contain the exact package members",
        ));
    }

    for _ in 0..expected.len() {
        if read_zip_u32(&mut archive)? != ZIP_CENTRAL_DIRECTORY_HEADER {
            return Err(invalid_zip("The ZIP central directory is incomplete"));
        }
        let _version_made_by = read_zip_u16(&mut archive)?;
        let version_needed = read_zip_u16(&mut archive)?;
        let flags = read_zip_u16(&mut archive)?;
        let method = read_zip_u16(&mut archive)?;
        let dos_time = read_zip_u16(&mut archive)?;
        let dos_date = read_zip_u16(&mut archive)?;
        let crc32 = read_zip_u32(&mut archive)?;
        let compressed_legacy = read_zip_u32(&mut archive)?;
        let uncompressed_legacy = read_zip_u32(&mut archive)?;
        let name_len = read_zip_u16(&mut archive)? as usize;
        let extra_len = read_zip_u16(&mut archive)? as usize;
        let comment_len = read_zip_u16(&mut archive)? as usize;
        let disk_number = read_zip_u16(&mut archive)?;
        let _internal_attributes = read_zip_u16(&mut archive)?;
        let _external_attributes = read_zip_u32(&mut archive)?;
        let offset_legacy = read_zip_u32(&mut archive)?;
        let name = read_zip_name(&mut archive, name_len)?;
        let extra = read_zip_bytes(&mut archive, extra_len)?;
        if comment_len != 0 {
            let _ = read_zip_bytes(&mut archive, comment_len)?;
        }
        let needs_uncompressed = uncompressed_legacy == u32::MAX;
        let needs_compressed = compressed_legacy == u32::MAX;
        let needs_offset = offset_legacy == u32::MAX;
        let (zip64_uncompressed, zip64_compressed, zip64_offset) =
            parse_zip64_extra(&extra, needs_uncompressed, needs_compressed, needs_offset)?;
        let uncompressed = if needs_uncompressed {
            zip64_uncompressed.ok_or_else(|| invalid_zip("The ZIP64 size is missing"))?
        } else {
            uncompressed_legacy as u64
        };
        let compressed = if needs_compressed {
            zip64_compressed.ok_or_else(|| invalid_zip("The ZIP64 size is missing"))?
        } else {
            compressed_legacy as u64
        };
        let offset = if needs_offset {
            zip64_offset.ok_or_else(|| invalid_zip("The ZIP64 offset is missing"))?
        } else {
            offset_legacy as u64
        };
        let Some((local, local_offset)) = local_members.get(&name) else {
            return Err(invalid_zip(
                "The ZIP central directory names an unknown member",
            ));
        };
        if !matches!(version_needed, 20 | 45)
            || flags != 0
            || method != 0
            || dos_time != 0
            || dos_date != 0x0021
            || disk_number != 0
            || compressed != uncompressed
            || uncompressed != local.size
            || crc32 != local.crc32
            || offset != *local_offset
        {
            return Err(invalid_zip(
                "The ZIP central directory does not match its members",
            ));
        }
    }
    let central_end = archive.stream_position().map_err(zip_read_error)?;
    let central_size = central_end - central_offset;
    let trailer_signature = read_zip_u32(&mut archive)?;
    let mut zip64_directory = None;
    let eocd_signature = if trailer_signature == ZIP64_END_OF_CENTRAL_DIRECTORY {
        let record_size = read_zip_u64(&mut archive)?;
        if record_size < 44 {
            return Err(invalid_zip("The ZIP64 end record is truncated"));
        }
        let _version_made_by = read_zip_u16(&mut archive)?;
        let _version_needed = read_zip_u16(&mut archive)?;
        let disk = read_zip_u32(&mut archive)?;
        let central_disk = read_zip_u32(&mut archive)?;
        let entries_on_disk = read_zip_u64(&mut archive)?;
        let entries = read_zip_u64(&mut archive)?;
        let declared_central_size = read_zip_u64(&mut archive)?;
        let declared_central_offset = read_zip_u64(&mut archive)?;
        if record_size > 44 {
            archive
                .seek(SeekFrom::Current((record_size - 44) as i64))
                .map_err(zip_read_error)?;
        }
        if read_zip_u32(&mut archive)? != ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR {
            return Err(invalid_zip("The ZIP64 locator is missing"));
        }
        let locator_disk = read_zip_u32(&mut archive)?;
        let zip64_offset = read_zip_u64(&mut archive)?;
        let total_disks = read_zip_u32(&mut archive)?;
        if disk != 0
            || central_disk != 0
            || locator_disk != 0
            || total_disks != 1
            || zip64_offset != central_end
            || entries_on_disk != expected.len() as u64
            || entries != expected.len() as u64
            || declared_central_size != central_size
            || declared_central_offset != central_offset
        {
            return Err(invalid_zip("The ZIP64 directory metadata is inconsistent"));
        }
        zip64_directory = Some((entries, declared_central_size, declared_central_offset));
        read_zip_u32(&mut archive)?
    } else {
        trailer_signature
    };
    if eocd_signature != ZIP_END_OF_CENTRAL_DIRECTORY {
        return Err(invalid_zip("The ZIP end-of-directory record is missing"));
    }
    let disk = read_zip_u16(&mut archive)?;
    let central_disk = read_zip_u16(&mut archive)?;
    let entries_on_disk = read_zip_u16(&mut archive)?;
    let entries = read_zip_u16(&mut archive)?;
    let declared_central_size = read_zip_u32(&mut archive)?;
    let declared_central_offset = read_zip_u32(&mut archive)?;
    let comment_len = read_zip_u16(&mut archive)?;
    if comment_len != 0
        || disk != 0
        || central_disk != 0
        || archive.stream_position().map_err(zip_read_error)? != archive_len
    {
        return Err(invalid_zip(
            "The ZIP end-of-directory record is inconsistent",
        ));
    }
    if zip64_directory.is_some() {
        if entries_on_disk != u16::MAX
            || entries != u16::MAX
            || declared_central_size != u32::MAX
            || declared_central_offset != u32::MAX
        {
            return Err(invalid_zip("The ZIP64 compatibility fields are invalid"));
        }
    } else if entries_on_disk as usize != expected.len()
        || entries as usize != expected.len()
        || declared_central_size as u64 != central_size
        || declared_central_offset as u64 != central_offset
    {
        return Err(invalid_zip("The ZIP directory metadata is inconsistent"));
    }
    Ok(())
}

fn crc32_file(path: &Path, cancel: Option<&AtomicBool>) -> ServiceResult<(u32, u64)> {
    let mut file = File::open(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_failed",
            "A package member could not be opened for checksumming",
            error,
        )
    })?;
    let mut state = 0xffff_ffff_u32;
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(VideoServiceError::cancelled());
        }
        let count = file.read(&mut buffer).map_err(|error| {
            VideoServiceError::io(
                "video.package_failed",
                "A package member could not be checksummed",
                error,
            )
        })?;
        if count == 0 {
            break;
        }
        state = crc32_update(state, &buffer[..count]);
        size = size.saturating_add(count as u64);
    }
    Ok((!state, size))
}

fn crc32_update(mut state: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        state ^= u32::from(*byte);
        for _ in 0..8 {
            state = (state >> 1) ^ (0xedb8_8320_u32 & 0_u32.wrapping_sub(state & 1));
        }
    }
    state
}

fn parse_zip64_extra(
    extra: &[u8],
    needs_uncompressed: bool,
    needs_compressed: bool,
    needs_offset: bool,
) -> ServiceResult<(Option<u64>, Option<u64>, Option<u64>)> {
    if !needs_uncompressed && !needs_compressed && !needs_offset {
        return Ok((None, None, None));
    }
    let mut cursor = 0_usize;
    while cursor.saturating_add(4) <= extra.len() {
        let kind = u16::from_le_bytes([extra[cursor], extra[cursor + 1]]);
        let length = u16::from_le_bytes([extra[cursor + 2], extra[cursor + 3]]) as usize;
        cursor += 4;
        let end = cursor
            .checked_add(length)
            .ok_or_else(|| invalid_zip("The ZIP extra field overflows"))?;
        if end > extra.len() {
            return Err(invalid_zip("The ZIP extra field is truncated"));
        }
        if kind == ZIP64_EXTRA_FIELD {
            let mut value_cursor = cursor;
            let mut next_u64 = || -> ServiceResult<u64> {
                let value_end = value_cursor
                    .checked_add(8)
                    .ok_or_else(|| invalid_zip("The ZIP64 extra field overflows"))?;
                if value_end > end {
                    return Err(invalid_zip("The ZIP64 extra field is truncated"));
                }
                let value = u64::from_le_bytes(
                    extra[value_cursor..value_end]
                        .try_into()
                        .map_err(|_| invalid_zip("The ZIP64 value is invalid"))?,
                );
                value_cursor = value_end;
                Ok(value)
            };
            let uncompressed = needs_uncompressed.then(&mut next_u64).transpose()?;
            let compressed = needs_compressed.then(&mut next_u64).transpose()?;
            let offset = needs_offset.then(&mut next_u64).transpose()?;
            return Ok((uncompressed, compressed, offset));
        }
        cursor = end;
    }
    Err(invalid_zip("The required ZIP64 extra field is missing"))
}

fn read_zip_name(reader: &mut File, length: usize) -> ServiceResult<String> {
    let bytes = read_zip_bytes(reader, length)?;
    let name = String::from_utf8(bytes)
        .map_err(|_| invalid_zip("A ZIP member name is not valid UTF-8"))?;
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(invalid_zip("A ZIP member name is unsafe"));
    }
    Ok(name)
}

fn read_zip_bytes(reader: &mut File, length: usize) -> ServiceResult<Vec<u8>> {
    if length > MAX_CAPTURE_BYTES {
        return Err(invalid_zip("A ZIP metadata field is unreasonably large"));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).map_err(zip_read_error)?;
    Ok(bytes)
}

fn write_zip_u16(writer: &mut File, value: u16) -> ServiceResult<()> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(zip_write_error)
}

fn write_zip_u32(writer: &mut File, value: u32) -> ServiceResult<()> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(zip_write_error)
}

fn write_zip_u64(writer: &mut File, value: u64) -> ServiceResult<()> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(zip_write_error)
}

fn push_zip_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn push_zip_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn read_zip_u16(reader: &mut File) -> ServiceResult<u16> {
    let mut bytes = [0_u8; 2];
    reader.read_exact(&mut bytes).map_err(zip_read_error)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_zip_u32(reader: &mut File) -> ServiceResult<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes).map_err(zip_read_error)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_zip_u64(reader: &mut File) -> ServiceResult<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes).map_err(zip_read_error)?;
    Ok(u64::from_le_bytes(bytes))
}

fn zip_write_error(error: std::io::Error) -> VideoServiceError {
    VideoServiceError::io(
        "video.package_failed",
        "The publish ZIP could not be written",
        error,
    )
}

fn zip_read_error(error: std::io::Error) -> VideoServiceError {
    VideoServiceError::io(
        "video.package_invalid",
        "The publish ZIP could not be read",
        error,
    )
}

fn invalid_zip(message: &'static str) -> VideoServiceError {
    VideoServiceError::new("video.package_invalid", message)
}

fn sync_directory(path: &Path) -> ServiceResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            VideoServiceError::io(
                "video.package_sync_failed",
                "The package directory could not be synchronized",
                error,
            )
        })
}

fn sync_directory_tree(path: &Path) -> ServiceResult<()> {
    for entry in fs::read_dir(path).map_err(|error| {
        VideoServiceError::io(
            "video.package_sync_failed",
            "The package directory could not be inspected",
            error,
        )
    })? {
        let entry = entry.map_err(|error| {
            VideoServiceError::io(
                "video.package_sync_failed",
                "A package entry could not be inspected",
                error,
            )
        })?;
        let metadata = entry.metadata().map_err(|error| {
            VideoServiceError::io(
                "video.package_sync_failed",
                "A package entry could not be inspected",
                error,
            )
        })?;
        if metadata.is_file() {
            File::open(entry.path())
                .and_then(|file| file.sync_all())
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.package_sync_failed",
                        "A package file could not be synchronized",
                        error,
                    )
                })?;
        }
    }
    sync_directory(path)
}

fn export_package_directory<F, G>(
    source: &Path,
    destination_parent: &Path,
    project_name: &str,
    cache_key: &str,
    cancel: &AtomicBool,
    before_publish: F,
) -> ServiceResult<PathBuf>
where
    F: FnOnce() -> ServiceResult<G>,
{
    let parent = fs::canonicalize(destination_parent).map_err(|error| {
        VideoServiceError::io(
            "video.export_destination_invalid",
            "The export destination could not be opened",
            error,
        )
    })?;
    if !parent.is_dir() {
        return Err(VideoServiceError::new(
            "video.export_destination_invalid",
            "The export destination must be an existing directory",
        ));
    }
    let slug = filename_slug(project_name);
    let final_path = parent.join(format!("soundar-{slug}-{}", &cache_key[..12]));
    if final_path.exists() {
        match validate_package_directory_with_cancel(&final_path, Some(cancel)) {
            Ok(()) if package_directories_equal(source, &final_path, Some(cancel))? => {
                let publication_guard = before_publish()?;
                drop(publication_guard);
                return Ok(final_path);
            }
            Err(error) if error.code == "video.cancelled" => return Err(error),
            _ => {}
        }
        return Err(VideoServiceError::new(
            "video.export_conflict",
            "The export destination already contains a conflicting package",
        ));
    }
    let inventory = package_member_inventory(source, Some(cancel))?;
    let aggregate_bytes = inventory
        .values()
        .fold(0_u64, |total, member| total.saturating_add(member.size));
    ensure_disk_capacity(
        &parent,
        with_disk_headroom(aggregate_bytes, 1),
        "publish_package_export",
    )?;
    let staging = parent.join(format!(".soundar-{slug}-{}.partial", new_id()));
    fs::create_dir(&staging).map_err(|error| {
        VideoServiceError::io(
            "video.export_failed",
            "The publish package export could not be staged",
            error,
        )
    })?;
    let copy_result = (|| -> ServiceResult<()> {
        for entry in fs::read_dir(source).map_err(|error| {
            VideoServiceError::io(
                "video.export_failed",
                "The managed package could not be read",
                error,
            )
        })? {
            let entry = entry.map_err(|error| {
                VideoServiceError::io(
                    "video.export_failed",
                    "A managed package entry could not be read",
                    error,
                )
            })?;
            let metadata = entry.metadata().map_err(|error| {
                VideoServiceError::io(
                    "video.export_failed",
                    "A managed package entry could not be inspected",
                    error,
                )
            })?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(VideoServiceError::new(
                    "video.export_failed",
                    "Managed publish packages may only contain regular files",
                ));
            }
            copy_file_verified(
                &entry.path(),
                &staging.join(entry.file_name()),
                CopiedFileVisibility::UserShareable,
                cancel,
            )?;
        }
        sync_directory_tree(&staging)?;
        validate_package_directory_with_cancel(&staging, Some(cancel))
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let publication_guard = match before_publish() {
        Ok(guard) => guard,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    fs::rename(&staging, &final_path).map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        VideoServiceError::io(
            "video.export_failed",
            "The publish package could not be atomically exported",
            error,
        )
    })?;
    sync_directory(&parent)?;
    drop(publication_guard);
    validate_package_directory_with_cancel(&final_path, Some(cancel))?;
    Ok(final_path)
}

fn package_directories_equal(
    left: &Path,
    right: &Path,
    cancel: Option<&AtomicBool>,
) -> ServiceResult<bool> {
    validate_package_directory_with_cancel(left, cancel)?;
    validate_package_directory_with_cancel(right, cancel)?;
    let inventory = |directory: &Path| -> ServiceResult<BTreeMap<String, String>> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(directory).map_err(|error| {
            VideoServiceError::io(
                "video.package_invalid",
                "The package directory could not be compared",
                error,
            )
        })? {
            let entry = entry.map_err(|error| {
                VideoServiceError::io(
                    "video.package_invalid",
                    "A package entry could not be compared",
                    error,
                )
            })?;
            let name = entry
                .file_name()
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.package_invalid",
                        "A package filename could not be compared",
                    )
                })?;
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return Err(VideoServiceError::cancelled());
            }
            files.insert(name, sha256_file_with_cancel(&entry.path(), cancel)?);
        }
        Ok(files)
    };
    Ok(inventory(left)? == inventory(right)?)
}

fn captions_to_srt(manifest: &VideoProjectManifest) -> ServiceResult<String> {
    let mut captions = manifest.captions.iter().collect::<Vec<_>>();
    captions.sort_by_key(|caption| caption.range.start_us);
    let mut output = String::new();
    for (index, caption) in captions.into_iter().enumerate() {
        use std::fmt::Write as _;
        writeln!(&mut output, "{}", index + 1).expect("String writes cannot fail");
        writeln!(
            &mut output,
            "{} --> {}",
            srt_timestamp(caption.range.start_us.0)?,
            srt_timestamp(caption.range.end_us.0)?
        )
        .expect("String writes cannot fail");
        writeln!(
            &mut output,
            "{}\n",
            caption.text.replace("\r\n", "\n").replace('\r', "\n")
        )
        .expect("String writes cannot fail");
    }
    Ok(output)
}

fn srt_timestamp(microseconds: i64) -> ServiceResult<String> {
    if microseconds < 0 {
        return Err(VideoServiceError::new(
            "video.invalid_caption",
            "Caption timestamps may not be negative",
        ));
    }
    let milliseconds = microseconds / 1_000;
    let hours = milliseconds / 3_600_000;
    let minutes = (milliseconds / 60_000) % 60;
    let seconds = (milliseconds / 1_000) % 60;
    let millis = milliseconds % 1_000;
    Ok(format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}"))
}

fn filename_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "video".to_string()
    } else {
        slug
    }
}

fn value_string(value: &Value, key: &str) -> ServiceResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_store_shape(key))
}

fn value_i64(value: &Value, key: &str) -> ServiceResult<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid_store_shape(key))
}

fn visual_extension(mime_type: VisualMimeType) -> &'static str {
    match mime_type {
        VisualMimeType::Png => "png",
        VisualMimeType::Jpeg => "jpg",
        VisualMimeType::Webp => "webp",
    }
}

fn visual_source_receipt(value: &Value) -> ServiceResult<VisualSourceReceipt> {
    let source_path = Path::new(&value_string(value, "source_path")?)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("visual")
        .to_string();
    Ok(VisualSourceReceipt {
        id: value_string(value, "id")?,
        receipt_kind: value_string(value, "receipt_kind")?,
        project_id: value_string(value, "project_id")?,
        expected_revision: value_i64(value, "expected_revision")?,
        expected_version_id: value_string(value, "expected_version_id")?,
        display_name: source_path,
        sha256: value_string(value, "sha256")?,
        mime_type: value_string(value, "mime_type")?,
        size_bytes: u64::try_from(value_i64(value, "size_bytes")?)
            .map_err(|_| invalid_store_shape("size_bytes"))?,
        width: u32::try_from(value_i64(value, "width")?)
            .map_err(|_| invalid_store_shape("width"))?,
        height: u32::try_from(value_i64(value, "height")?)
            .map_err(|_| invalid_store_shape("height"))?,
        expires_at: value_string(value, "expires_at")?,
    })
}

fn project_expectation(project: &Value) -> ServiceResult<ProjectExpectation> {
    let revision = value_i64(project, "revision")?;
    let version_id = project
        .get("version")
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| invalid_store_shape("version.id"))?;
    Ok(ProjectExpectation {
        revision,
        version_id,
    })
}

fn ensure_project_matches(project: &Value, expectation: &ProjectExpectation) -> ServiceResult<()> {
    let current = project_expectation(project)?;
    if current != *expectation {
        return Err(VideoServiceError::new(
            "video.revision_conflict",
            "The project changed while this video task was running; the stale result was not published",
        )
        .retryable(true)
        .details(json!({
            "expected_revision": expectation.revision,
            "actual_revision": current.revision,
            "expected_version_id": expectation.version_id,
            "actual_version_id": current.version_id,
        })));
    }
    Ok(())
}

fn expectation_from_optional(
    project: &Value,
    expected_revision: Option<i64>,
    expected_version_id: Option<&str>,
) -> ServiceResult<ProjectExpectation> {
    let requested = declared_expectation(project, expected_revision, expected_version_id)?;
    ensure_project_matches(project, &requested)?;
    Ok(requested)
}

/// Parses an optional revision/version pair without asserting that it is the
/// current project version. Durable render runners need this narrow form to
/// recognize an exact atomic publication that committed before the worker
/// could persist its terminal job state.
fn declared_expectation(
    project: &Value,
    expected_revision: Option<i64>,
    expected_version_id: Option<&str>,
) -> ServiceResult<ProjectExpectation> {
    match (expected_revision, expected_version_id) {
        (None, None) => project_expectation(project),
        (Some(revision), Some(version_id)) => Ok(ProjectExpectation {
            revision,
            version_id: version_id.to_string(),
        }),
        _ => Err(VideoServiceError::new(
            "video.invalid_revision_expectation",
            "expected_revision and expected_version_id must be supplied together",
        )),
    }
}

fn normalize_utc_timestamp(value: &str) -> ServiceResult<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .map_err(|error| {
            VideoServiceError::new(
                "video.invalid_rights_receipt",
                "The saved rights confirmation timestamp is invalid",
            )
            .details(json!({ "diagnostic": error.to_string() }))
        })
}

fn manifest_content_value(manifest: &VideoProjectManifest) -> ServiceResult<Value> {
    let mut value = serde_json::to_value(manifest).map_err(json_error)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid_store_shape("manifest"))?;
    // These fields describe the commit itself and therefore cannot establish
    // what content the commit changed.
    object.remove("revision");
    object.remove("revision_history");
    object.remove("updated_at");
    Ok(value)
}

fn timeline_caption_cache_key(
    manifest: &VideoProjectManifest,
    request: &TimelineRenderRequest,
) -> ServiceResult<String> {
    CacheKeyBuilder::new(CacheStage::Captions, format!("{SERVICE_VERSION}:ass-v3"))
        .manifest_slice(json!({
            "reviewed_scenes": &manifest.reviewed_scenes,
            "captions": &manifest.captions,
            // Exact karaoke/highlight timing is derived through these canonical source-clock
            // inputs. Binding them prevents a corrected transcript or retimed clip from reusing
            // an ASS document produced for older word mappings.
            "transcript": &manifest.transcript,
            "tracks": &manifest.tracks,
            "timeline_duration_us": manifest.timeline_duration_us,
            "frame_rate": manifest.frame_rate,
            // ASS PlayRes, font sizing, and vertical margins are layout-aware.
            // Bind the complete layout so future layout-driven overlay changes
            // cannot silently reuse a caption document from another canvas.
            "layout": &manifest.layout,
        }))
        .profile(json!({
            "profile": request.profile,
            "caption_theme": request.caption_theme,
            "include_title_cards": request.include_title_cards,
            "include_speaker_cards": request.include_speaker_cards,
            "burn_captions": request.burn_captions,
        }))
        .build()
        .map(|key| key.into_string())
        .map_err(VideoServiceError::from)
}

fn effective_timeline_variation_request(request: &TimelineRenderRequest) -> TimelineRenderRequest {
    let mut effective = request.clone();
    if request.variation == 0 {
        return effective;
    }
    let layout_index = match request.portrait_layout {
        PortraitSourceLayout::CenterCrop => 0,
        PortraitSourceLayout::Contain => 1,
        PortraitSourceLayout::BlurPad => 2,
    };
    let theme_index = match request.caption_theme {
        CaptionTheme::CleanWhite => 0,
        CaptionTheme::Calm => 1,
        CaptionTheme::Kinetic => 2,
    };
    let discriminator = usize::from(request.variation);
    effective.portrait_layout = match (layout_index + discriminator % 3) % 3 {
        0 => PortraitSourceLayout::CenterCrop,
        1 => PortraitSourceLayout::Contain,
        _ => PortraitSourceLayout::BlurPad,
    };
    effective.caption_theme = match (theme_index + (discriminator / 3) % 3) % 3 {
        0 => CaptionTheme::CleanWhite,
        1 => CaptionTheme::Calm,
        _ => CaptionTheme::Kinetic,
    };
    effective
}

fn escape_json_pointer_component(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn collect_manifest_diff(
    before: &Value,
    after: &Value,
    path: &str,
    changed: &mut BTreeSet<String>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            let keys = before
                .keys()
                .chain(after.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = format!("{path}/{}", escape_json_pointer_component(&key));
                match (before.get(&key), after.get(&key)) {
                    (Some(left), Some(right)) => {
                        collect_manifest_diff(left, right, &child_path, changed)
                    }
                    _ => {
                        changed.insert(child_path);
                    }
                }
            }
        }
        // Arrays are versioned contract collections. Treating a collection as
        // one change avoids brittle index paths when an item is inserted or
        // reordered, while still binding the declaration to the actual field.
        (Value::Array(_), Value::Array(_)) => {
            changed.insert(if path.is_empty() { "/" } else { path }.to_string());
        }
        _ => {
            changed.insert(if path.is_empty() { "/" } else { path }.to_string());
        }
    }
}

/// Return the canonical JSON-pointer paths for versioned manifest content changes.
///
/// Every command surface must use this implementation so revision declarations cannot drift
/// from the optimistic-CAS validation performed by [`VideoStudioService::revise_manifest`].
pub(crate) fn manifest_changed_paths(
    before: &VideoProjectManifest,
    after: &VideoProjectManifest,
) -> ServiceResult<BTreeSet<String>> {
    let before_caption_elements = before
        .layout
        .elements
        .iter()
        .filter(|element| matches!(element.role, LayoutRole::Captions))
        .collect::<Vec<_>>();
    let after_caption_elements = after
        .layout
        .elements
        .iter()
        .filter(|element| matches!(element.role, LayoutRole::Captions))
        .collect::<Vec<_>>();
    let before_other_elements = before
        .layout
        .elements
        .iter()
        .filter(|element| !matches!(element.role, LayoutRole::Captions))
        .collect::<Vec<_>>();
    let after_other_elements = after
        .layout
        .elements
        .iter()
        .filter(|element| !matches!(element.role, LayoutRole::Captions))
        .collect::<Vec<_>>();
    let before = manifest_content_value(before)?;
    let after = manifest_content_value(after)?;
    let mut changed = BTreeSet::new();
    collect_manifest_diff(&before, &after, "", &mut changed);
    if changed.remove("/layout/elements") {
        if before_caption_elements != after_caption_elements {
            changed.insert("/layout/elements/captions".to_string());
        }
        if before_other_elements != after_other_elements {
            changed.insert("/layout/elements".to_string());
        }
    }
    Ok(changed)
}

/// Derive the exact invalidation set required for canonical manifest change paths.
pub(crate) fn invalidated_stages_for_manifest_changes(
    paths: &BTreeSet<String>,
) -> BTreeSet<RevisionStage> {
    let all = BTreeSet::from([
        RevisionStage::Ingest,
        RevisionStage::Transcript,
        RevisionStage::Analysis,
        RevisionStage::Plan,
        RevisionStage::Speech,
        RevisionStage::Music,
        RevisionStage::Captions,
        RevisionStage::Tracking,
        RevisionStage::SceneRender,
        RevisionStage::Preview,
        RevisionStage::FinalRender,
        RevisionStage::PublishPackage,
    ]);
    let mut stages = BTreeSet::new();
    for path in paths {
        let inferred = if path == "/" || path.starts_with("/schema_version") {
            all.clone()
        } else if path.starts_with("/source_assets") {
            BTreeSet::from([
                RevisionStage::Ingest,
                RevisionStage::Transcript,
                RevisionStage::Analysis,
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/transcript") {
            BTreeSet::from([
                RevisionStage::Transcript,
                RevisionStage::Analysis,
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/candidates") {
            BTreeSet::from([
                RevisionStage::Analysis,
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/reviewed_scenes")
            || path.starts_with("/tracks")
            || path.starts_with("/gaps")
            || path.starts_with("/timeline_duration_us")
            || path.starts_with("/frame_rate")
        {
            BTreeSet::from([
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/captions") || path.starts_with("/layout/elements/captions") {
            BTreeSet::from([
                RevisionStage::Captions,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/visual_assets") || path.starts_with("/visual_layers") {
            BTreeSet::from([
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/layout") {
            BTreeSet::from([
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/audio_mix") {
            BTreeSet::from([
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/turn_beats") || path.starts_with("/performance_clock") {
            // Retiming a conversation changes how the takes are assembled, not what any voice was
            // asked to say. Invalidating Speech here would re-read every line to move one pause.
            BTreeSet::from([
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/cast")
            || path.starts_with("/dialogue")
            || path.starts_with("/lexicon")
        {
            // A cast or script change can only invalidate performed speech and everything
            // assembled from it. Ingest, transcript, and analysis describe imported source and
            // are deliberately untouched by rewriting what the characters say.
            BTreeSet::from([
                RevisionStage::Speech,
                RevisionStage::Captions,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/narration_bindings") {
            BTreeSet::from([
                RevisionStage::Speech,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        } else if path.starts_with("/rights_confirmations") || path.starts_with("/name") {
            BTreeSet::from([RevisionStage::PublishPackage])
        } else {
            BTreeSet::new()
        };
        stages.extend(inferred);
    }
    stages
}

fn invalid_store_shape(field: &str) -> VideoServiceError {
    VideoServiceError::new(
        "video.store_contract_failed",
        "The persistent video record has an invalid shape",
    )
    .details(json!({ "field": field }))
}

fn json_error(error: serde_json::Error) -> VideoServiceError {
    VideoServiceError::new(
        "video.invalid_manifest",
        "The video contract could not be serialized or parsed",
    )
    .details(json!({ "diagnostic": error.to_string() }))
}

fn parse_resume_request<T>(mut value: Value) -> ServiceResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    // Older local-import jobs included transport-only scheduling metadata.
    // It is intentionally ignored by every owning workflow runner.
    if let Some(object) = value.as_object_mut() {
        object.remove("priority");
    }
    serde_json::from_value(value).map_err(|error| {
        VideoServiceError::new(
            "video.resume_request_invalid",
            "The saved Video Studio task request is incomplete or invalid",
        )
        .details(json!({ "diagnostic": error.to_string() }))
    })
}

fn require_text<'a>(
    value: &'a str,
    code: &'static str,
    message: &'static str,
) -> ServiceResult<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        Err(VideoServiceError::new(code, message))
    } else {
        Ok(value)
    }
}

fn narration_replacements_applied(
    manifest: &VideoProjectManifest,
    replacements: &[NarrationReplacement],
) -> bool {
    replacements.iter().all(|replacement| {
        let binding = if let Some(binding_id) = replacement.binding_id.as_deref() {
            manifest
                .narration_bindings
                .iter()
                .find(|binding| binding.id == binding_id)
        } else if let Some(scene_id) = replacement.scene_id.as_deref() {
            manifest
                .narration_bindings
                .iter()
                .find(|binding| binding.scene_id.as_deref() == Some(scene_id))
        } else if let Some(clip_id) = replacement.clip_id.as_deref() {
            manifest
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .find(|clip| clip.id == clip_id)
                .and_then(|clip| clip.media.render_artifact_id.as_deref())
                .and_then(|artifact_id| {
                    manifest
                        .narration_bindings
                        .iter()
                        .find(|binding| binding.render_artifact_id == artifact_id)
                })
        } else {
            None
        };
        let Some(binding) = binding else {
            return false;
        };
        let clip_matches = replacement.clip_id.as_deref().is_none_or(|clip_id| {
            manifest
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .any(|clip| {
                    clip.id == clip_id
                        && clip.media.render_artifact_id.as_deref()
                            == Some(binding.render_artifact_id.as_str())
                })
        });
        clip_matches
            && replacement
                .scene_id
                .as_deref()
                .is_none_or(|scene_id| binding.scene_id.as_deref() == Some(scene_id))
            && binding.history_id == replacement.history_id
            && binding.voice_id == replacement.voice_id
            && binding.model_id == replacement.model_id
            && binding.speaker == replacement.speaker
            && binding.language == replacement.language
    })
}

fn atempo_filter_chain(rate: f64) -> ServiceResult<String> {
    if !rate.is_finite() || !(0.125..=8.0).contains(&rate) {
        return Err(VideoServiceError::new(
            "video.narration_duration_incompatible",
            "The replacement speech duration is too different from the reviewed scene timing",
        )
        .details(json!({ "tempo_ratio": rate })));
    }
    let mut remaining = rate;
    let mut filters = Vec::new();
    while remaining > 2.0 {
        filters.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        filters.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    filters.push(format!("atempo={remaining:.8}"));
    Ok(filters.join(","))
}

fn validate_safe_component(value: &str, code: &'static str) -> ServiceResult<()> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(VideoServiceError::new(
            code,
            "The identifier contains unsupported characters",
        ));
    }
    Ok(())
}

fn redact_publish_manifest_urls(manifest: &VideoProjectManifest) -> (VideoProjectManifest, bool) {
    let mut published = manifest.clone();
    let mut redacted = false;
    for source in &mut published.source_assets {
        if let Some(uri) = source.provenance.original_uri.as_mut() {
            let (safe, changed) = publish_safe_source_uri(uri);
            *uri = safe;
            redacted |= changed;
        }
    }
    for receipt in &mut published.rights_confirmations {
        let (safe, changed) = publish_safe_source_uri(&receipt.source_uri);
        receipt.source_uri = safe;
        redacted |= changed;
        // `source_uri_sha256` deliberately remains bound to the exact URL that
        // was authorized. The exported display URI is privacy-redacted only.
    }
    (published, redacted)
}

fn publish_safe_source_uri(uri: &str) -> (String, bool) {
    if !uri.starts_with("https://") || !uri.contains('?') {
        return (uri.to_string(), false);
    }
    if let Ok(validated) = validate_import_url(uri) {
        // A canonical YouTube `v` value is public source identity, not an
        // access credential. Extra tracking parameters were already removed at
        // ingest, so retain the useful, canonical reference in publish exports.
        if validated.source_id.is_some() {
            return (validated.canonical, false);
        }
    }
    let public_path = uri
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(uri)
        .split('#')
        .next()
        .unwrap_or_default();
    (public_path.to_string(), true)
}

fn validate_hash(value: &str) -> ServiceResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(VideoServiceError::new(
            "video.invalid_cache_key",
            "The cache key must be a lowercase SHA-256 value",
        ));
    }
    Ok(())
}

fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn managed_duration_seconds_from_project(
    service: &VideoStudioService,
    project_id: &str,
) -> ServiceResult<f64> {
    let project = service.get_project(project_id)?;
    let duration_us = project
        .get("manifest")
        .and_then(|value| value.get("timeline_duration_us"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.get(0).and_then(Value::as_i64))
        })
        .or_else(|| project.get("duration_us").and_then(Value::as_i64))
        .ok_or_else(|| invalid_store_shape("duration_us"))?;
    Ok(duration_us as f64 / 1_000_000.0)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::super::{
        AudioMix, CanvasMode, CanvasSpec, CaptionCue, GapReason, LayoutPlan, MediaReference,
        NormalizedRect, ReviewState, ReviewedScene, TimelineGap,
    };
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct RecordingGpuState {
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        attempts: AtomicUsize,
        releases: AtomicUsize,
        blocked: AtomicBool,
        requests: Mutex<Vec<Value>>,
    }

    struct RecordingGpuGate {
        state: Arc<RecordingGpuState>,
    }

    struct RecordingGpuLease {
        state: Arc<RecordingGpuState>,
    }

    impl SharedGpuAdmissionLease for RecordingGpuLease {}

    impl Drop for RecordingGpuLease {
        fn drop(&mut self) {
            self.state.active.fetch_sub(1, Ordering::AcqRel);
            self.state.releases.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl SharedGpuAdmissionGate for RecordingGpuGate {
        fn try_acquire(
            &self,
            request: &SharedGpuAdmissionRequest,
        ) -> ServiceResult<SharedGpuAdmissionOutcome> {
            self.state.attempts.fetch_add(1, Ordering::AcqRel);
            self.state
                .requests
                .lock()
                .expect("recording gate requests")
                .push(serde_json::to_value(request).expect("serializable GPU request"));
            if self.state.blocked.load(Ordering::Acquire)
                || self
                    .state
                    .active
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return Ok(SharedGpuAdmissionOutcome::Waiting(SharedGpuAdmissionWait {
                    reason: "another GPU workload is active".to_string(),
                    retry_after_ms: 10,
                    details: Some(json!({ "fake": true })),
                }));
            }
            let active = self.state.active.load(Ordering::Acquire);
            let mut maximum = self.state.maximum_active.load(Ordering::Acquire);
            while active > maximum {
                match self.state.maximum_active.compare_exchange_weak(
                    maximum,
                    active,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(observed) => maximum = observed,
                }
            }
            Ok(SharedGpuAdmissionOutcome::admitted(RecordingGpuLease {
                state: Arc::clone(&self.state),
            }))
        }
    }

    struct TestWorkspace {
        root: PathBuf,
        service: Arc<VideoStudioService>,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("soundar-video-service-{}", new_id()));
            fs::create_dir_all(&root).expect("test workspace");
            let store = Arc::new(
                Store::open(root.join("data"), root.join("artifacts")).expect("test store"),
            );
            let service = Arc::new(VideoStudioService::new(store).expect("video service"));
            Self { root, service }
        }

        fn with_gpu_gate(gate: Arc<dyn SharedGpuAdmissionGate>) -> Self {
            let root = std::env::temp_dir().join(format!("soundar-video-service-{}", new_id()));
            fs::create_dir_all(&root).expect("test workspace");
            let store = Arc::new(
                Store::open(root.join("data"), root.join("artifacts")).expect("test store"),
            );
            let service = Arc::new(
                VideoStudioService::new_with_gpu_admission_gate(store, gate)
                    .expect("video service with shared GPU gate"),
            );
            Self { root, service }
        }

        fn create_project(&self, project_id: &str, name: &str) -> Value {
            let manifest = empty_manifest(project_id, name);
            self.service
                .create_project(CreateVideoProjectRequest {
                    name: name.to_string(),
                    manifest,
                    actor: "service-test".to_string(),
                    initial_intent: Some("A compact animated podcast".to_string()),
                })
                .expect("create project")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            // Every test root includes a fresh UUID and is never derived from an
            // environment variable or caller-controlled path.
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn empty_manifest(project_id: &str, name: &str) -> VideoProjectManifest {
        VideoProjectManifest::new(
            project_id,
            name,
            RationalFrameRate::FPS_30,
            Microseconds(1_000_000),
            LayoutPlan {
                mode: CanvasMode::Portrait,
                canvas: CanvasSpec {
                    width: 1080,
                    height: 1920,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 9_000,
                    height_bp: 9_000,
                },
                background_rgba: [244, 244, 243, 255],
                elements: Vec::new(),
            },
            AudioMix {
                target_lufs_milli: -16_000,
                true_peak_db_milli: -1_000,
                tracks: Vec::new(),
            },
            utc_now(),
        )
        .expect("valid empty manifest")
    }

    fn performed_turn_clip(index: usize, start_us: i64, duration_us: i64) -> TimelineClip {
        TimelineClip {
            id: format!("dialogue-clip-{index}"),
            scene_id: None,
            turn_id: Some(format!("turn-{index:04}")),
            media: MediaReference {
                source_asset_id: None,
                render_artifact_id: Some(format!("take-{index:04}")),
            },
            source_range: TimeRange::new(0, duration_us).expect("take range"),
            timeline_start_us: Microseconds(start_us),
            timeline_duration_us: Microseconds(duration_us),
            playback_rate: RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        }
    }

    fn scene_for_dialogue() -> ReviewedScene {
        ReviewedScene {
            id: "dialogue-scene".to_string(),
            candidate_id: None,
            source_asset_id: None,
            source_range: None,
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(1),
            title: "Episode".to_string(),
            script: "A performed conversation".to_string(),
            review_state: ReviewState::Approved,
            revision: 1,
        }
    }

    fn test_visual_asset(id: &str) -> VisualAsset {
        VisualAsset {
            id: id.to_string(),
            managed_path: "cache/cover/card.png".to_string(),
            sha256: "a".repeat(64),
            mime_type: VisualMimeType::Png,
            width: 1920,
            height: 1080,
            has_alpha: false,
            size_bytes: 4_096,
            provenance: Provenance {
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: utc_now(),
                producer: "soundAr cover".to_string(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            created_at: utc_now(),
        }
    }

    #[test]
    fn a_conversation_backs_its_own_scene_even_though_it_is_many_takes() {
        let mut manifest = empty_manifest("project-cover", "Episode");
        // Two lines with a beat between them: the silence is the performance's timing, not a hole.
        manifest.tracks = vec![TimelineTrack {
            id: DIALOGUE_TRACK_ID.to_string(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: vec![
                performed_turn_clip(0, 0, 4_000_000),
                performed_turn_clip(1, 4_220_000, 3_000_000),
            ],
        }];
        let scene = ReviewedScene {
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(7_220_000),
            ..scene_for_dialogue()
        };
        assert!(scene_is_covered_by_its_turns(&manifest, &scene));

        // A scene claiming time past the last line is not accounted for by the performance.
        let overlong = ReviewedScene {
            timeline_duration_us: Microseconds(9_000_000),
            ..scene.clone()
        };
        assert!(!scene_is_covered_by_its_turns(&manifest, &overlong));
    }

    #[test]
    fn an_unperformed_line_does_not_back_a_scene() {
        let mut unperformed = performed_turn_clip(1, 4_220_000, 3_000_000);
        // A line with no take has nothing to render, so it cannot count as covering anything.
        unperformed.media.render_artifact_id = None;
        let mut manifest = empty_manifest("project-cover", "Episode");
        manifest.tracks = vec![TimelineTrack {
            id: DIALOGUE_TRACK_ID.to_string(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: vec![performed_turn_clip(0, 0, 4_000_000), unperformed],
        }];
        let scene = ReviewedScene {
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(7_220_000),
            ..scene_for_dialogue()
        };
        assert!(!scene_is_covered_by_its_turns(&manifest, &scene));
    }

    #[test]
    fn a_drawn_card_never_counts_as_the_episode_having_a_picture() {
        let mut manifest = empty_manifest("project-cover", "Episode");
        assert!(!episode_has_own_picture(&manifest));

        let mut card = test_visual_asset("cover-abcdef0123456789abcdef01");
        card.provenance.kind = ProvenanceKind::GeneratedLocally;
        manifest.visual_assets.push(card);
        // Otherwise soundAr would treat its own fallback as a reason not to draw a better one.
        assert!(!episode_has_own_picture(&manifest));

        let mut chosen = test_visual_asset("still-abcdef0123456789abcdef01");
        chosen.provenance.kind = ProvenanceKind::UserUpload;
        manifest.visual_assets.push(chosen);
        assert!(episode_has_own_picture(&manifest));
    }

    fn test_png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0_u8; 30];
        bytes[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        bytes[8..12].copy_from_slice(&13_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes[24] = 8;
        bytes[25] = 6;
        bytes
    }

    fn test_visual_motion() -> VisualMotion {
        VisualMotion {
            start_bounds: NormalizedRect {
                x_bp: 0,
                y_bp: 0,
                width_bp: 10_000,
                height_bp: 10_000,
            },
            end_bounds: NormalizedRect {
                x_bp: 0,
                y_bp: 0,
                width_bp: 10_000,
                height_bp: 10_000,
            },
            start_opacity_milli: 1_000,
            end_opacity_milli: 1_000,
            start_rotation_milli_degrees: 0,
            end_rotation_milli_degrees: 0,
            easing: super::super::VisualEasing::Linear,
        }
    }

    #[test]
    fn creation_and_revision_keep_store_and_manifest_aligned() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Revision contract");
        assert_eq!(created.get("revision").and_then(Value::as_i64), Some(1));
        let mut manifest: VideoProjectManifest =
            serde_json::from_value(created.get("manifest").cloned().expect("manifest"))
                .expect("typed manifest");
        assert_eq!(manifest.revision, 1);
        assert_eq!(manifest.revision_history.len(), 1);
        manifest.name = "Revision contract updated".to_string();
        manifest.revision = 2;
        let parent = manifest
            .revision_history
            .last()
            .map(|record| record.id.clone());
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: 2,
            parent_id: parent,
            actor: "service-test".to_string(),
            reason: "Rename project".to_string(),
            changed_paths: vec!["/name".to_string()],
            invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
            created_at: utc_now(),
        });
        manifest.updated_at = utc_now();
        manifest.validate_strict().expect("replacement is strict");
        let revised = workspace
            .service
            .revise_manifest(ReviseVideoManifestRequest {
                project_id,
                expected_revision: 1,
                manifest,
                actor: "service-test".to_string(),
                reason: "Rename project".to_string(),
                changed_paths: vec!["/name".to_string()],
                invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
                status: Some("draft".to_string()),
            })
            .expect("optimistic revision");
        assert_eq!(revised.get("revision").and_then(Value::as_i64), Some(2));
        assert_eq!(
            revised
                .get("manifest")
                .and_then(|value| value.get("revision"))
                .and_then(Value::as_u64),
            Some(2)
        );
    }

    #[test]
    fn timeline_edit_is_version_bound_atomic_and_idempotent() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let mut manifest = empty_manifest(&project_id, "Timeline edit contract");
        manifest.reviewed_scenes = vec![
            ReviewedScene {
                id: "scene-one".to_string(),
                candidate_id: None,
                source_asset_id: None,
                source_range: None,
                timeline_start_us: Microseconds(0),
                timeline_duration_us: Microseconds(500_000),
                title: "Opening".to_string(),
                script: "Opening".to_string(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
            ReviewedScene {
                id: "scene-two".to_string(),
                candidate_id: None,
                source_asset_id: None,
                source_range: None,
                timeline_start_us: Microseconds(500_000),
                timeline_duration_us: Microseconds(500_000),
                title: "Close".to_string(),
                script: "Close".to_string(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
        ];
        manifest.validate_strict().expect("editable manifest");
        let created = workspace
            .service
            .create_project(CreateVideoProjectRequest {
                name: manifest.name.clone(),
                manifest,
                actor: "service-test".to_string(),
                initial_intent: Some("Exercise durable timeline editing".to_string()),
            })
            .expect("create editable project");
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();
        let request = VideoTimelineEditRequest {
            project_id: project_id.clone(),
            expected_revision: 1,
            base_version_id: version_id,
            operation_id: "timeline-operation-reorder".to_string(),
            operations: vec![super::super::VideoTimelineOperation::ReorderScene {
                scene_id: "scene-two".to_string(),
                to_index: 0,
            }],
        };

        let edited = workspace
            .service
            .edit_timeline(request.clone())
            .expect("commit timeline edit");
        assert!(!edited.replayed);
        assert_eq!(
            edited.project.get("revision").and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            edited
                .project
                .pointer("/manifest/reviewed_scenes/0/id")
                .and_then(Value::as_str),
            Some("scene-two")
        );
        assert_eq!(
            workspace
                .service
                .store
                .get_job(&edited.job_id)
                .expect("edit job")
                .and_then(|job| job.get("status").cloned())
                .and_then(|status| status.as_str().map(str::to_string))
                .as_deref(),
            Some("completed")
        );

        let replay = workspace
            .service
            .edit_timeline(request.clone())
            .expect("adopt exact edit replay");
        assert!(replay.replayed);
        assert_eq!(replay.job_id, edited.job_id);
        assert_eq!(
            replay.project.get("revision").and_then(Value::as_i64),
            Some(2),
            "an idempotent replay must not create another version"
        );

        let mut conflicting = request;
        conflicting.operations = vec![super::super::VideoTimelineOperation::ReorderScene {
            scene_id: "scene-one".to_string(),
            to_index: 0,
        }];
        let error = workspace
            .service
            .edit_timeline(conflicting)
            .expect_err("operation identifiers cannot be reused with different edits");
        assert_eq!(error.code, "video.idempotency_conflict");
    }

    fn script_cast() -> Vec<CastMember> {
        vec![
            CastMember {
                id: "narrator".to_string(),
                name: "NARRATOR".to_string(),
                display_name: "Narrator".to_string(),
                voice_id: "af-heart".to_string(),
                model_id: "hexgrad/Kokoro-82M".to_string(),
                language: "en-US".to_string(),
                delivery: super::super::CastDelivery::default(),
                consent_reference_id: None,
                persona: None,
                ensemble: 1,
                notes: None,
                created_at: utc_now(),
            },
            CastMember {
                id: "adaeze".to_string(),
                name: "ADAEZE".to_string(),
                display_name: "Adaeze".to_string(),
                voice_id: "af-bella".to_string(),
                model_id: "hexgrad/Kokoro-82M".to_string(),
                language: "en-US".to_string(),
                delivery: super::super::CastDelivery::default(),
                consent_reference_id: None,
                persona: None,
                ensemble: 1,
                notes: None,
                created_at: utc_now(),
            },
        ]
    }

    fn show_format() -> ShowFormat {
        use super::super::{CanvasMode, CanvasSpec, PerformanceClock};
        ShowFormat {
            id: "show-harmattan".to_string(),
            name: "The Harmattan Letters".to_string(),
            revision: 0,
            cast: script_cast(),
            lexicon: Vec::new(),
            performance_clock: PerformanceClock::default(),
            caption_preset_id: "podcast".to_string(),
            canvas_mode: CanvasMode::Portrait,
            canvas: CanvasSpec {
                width: 1080,
                height: 1920,
                pixel_aspect_numerator: 1,
                pixel_aspect_denominator: 1,
            },
            frame_rate: RationalFrameRate::FPS_30,
            target_lufs_milli: -16_000,
            true_peak_db_milli: -1_000,
            target_duration_us: Microseconds(600_000_000),
            opening: None,
            closing: None,
            show_notes_style: None,
            created_at: utc_now(),
            updated_at: utc_now(),
        }
    }

    #[test]
    fn a_generated_scene_tracks_the_performance_without_churning_a_matching_one() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let mut manifest = empty_manifest(&project_id, "Episode");
        let spoken_end = 7_220_000;
        manifest.cast.push(crate::video::cast::CastMember {
            id: "ch-mara".to_string(),
            name: "MARA".to_string(),
            display_name: "Mara".to_string(),
            voice_id: "af_heart".to_string(),
            model_id: "hexgrad/Kokoro-82M".to_string(),
            language: "en-US".to_string(),
            delivery: Default::default(),
            consent_reference_id: None,
            persona: None,
            ensemble: 1,
            notes: None,
            created_at: utc_now(),
        });
        for index in 0..2 {
            manifest.render_artifacts.push(RenderArtifact {
                id: format!("take-{index:04}"),
                role: RenderArtifactRole::SceneSegment,
                scene_id: None,
                managed_path: format!("cache/narration/take-{index:04}.wav"),
                sha256: format!("{index:064}"),
                cache_key: format!("{index:064}"),
                mime_type: "audio/wav".to_string(),
                duration_us: Some(Microseconds(4_000_000)),
                width: None,
                height: None,
                publication_state: PublicationState::Published,
                created_at: utc_now(),
            });
            manifest.dialogue.push(crate::video::cast::DialogueTurn {
                id: format!("turn-{index:04}"),
                scene_id: None,
                order: index,
                character_id: "ch-mara".to_string(),
                text: format!("Line {index}."),
                direction: None,
                source_line: index + 1,
                revision: 1,
            });
        }
        manifest.tracks = vec![TimelineTrack {
            id: DIALOGUE_TRACK_ID.to_string(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: vec![
                performed_turn_clip(0, 0, 4_000_000),
                performed_turn_clip(1, 4_220_000, 3_000_000),
            ],
        }];
        // An episode whose scene was built before the length rule existed carries its show
        // format's planning target rather than what was actually performed.
        manifest.reviewed_scenes.push(ReviewedScene {
            timeline_duration_us: Microseconds(600_000_000),
            ..scene_for_dialogue()
        });
        manifest.timeline_duration_us = Microseconds(600_000_000);
        let created = workspace
            .service
            .create_project(CreateVideoProjectRequest {
                name: "Episode".to_string(),
                manifest,
                actor: "service-test".to_string(),
                initial_intent: None,
            })
            .expect("create the episode");

        let corrected = workspace
            .service
            .ensure_dialogue_scene(&project_id, "service-test")
            .expect("bring the scene back in step");
        let after: VideoProjectManifest =
            serde_json::from_value(corrected.get("manifest").cloned().expect("manifest"))
                .expect("decode");
        // The episode is as long as it was performed, not as long as the show usually runs.
        assert_eq!(after.timeline_duration_us, Microseconds(spoken_end));
        assert_eq!(after.reviewed_scenes.len(), 1);
        assert_eq!(
            after.reviewed_scenes[0].timeline_duration_us,
            Microseconds(spoken_end)
        );
        assert!(
            value_i64(&corrected, "revision").expect("revision")
                > value_i64(&created, "revision").expect("revision")
        );

        // Asking again changes nothing, so a re-narration cannot churn the revision and invalidate
        // every render that depends on it.
        let again = workspace
            .service
            .ensure_dialogue_scene(&project_id, "service-test")
            .expect("already in step");
        assert_eq!(
            value_i64(&again, "revision").expect("revision"),
            value_i64(&corrected, "revision").expect("revision")
        );
    }

    #[test]
    fn a_cover_is_drawn_once_redrawn_on_request_and_replaced_when_its_file_goes_missing() {
        let workspace = TestWorkspace::new();
        // Drawing needs FFmpeg; without it there is nothing to assert about a drawn card.
        if !workspace.service.runtime_status(false).ffmpeg.available {
            return;
        }
        let project_id = format!("project-{}", new_id());
        let mut manifest = empty_manifest(&project_id, "The Quiet Server");
        // A card covers a performance, so the episode needs one to cover.
        manifest.reviewed_scenes.push(ReviewedScene {
            timeline_duration_us: Microseconds(17_580_000),
            ..scene_for_dialogue()
        });
        manifest.timeline_duration_us = Microseconds(17_580_000);
        workspace
            .service
            .create_project(CreateVideoProjectRequest {
                name: "The Quiet Server".to_string(),
                manifest,
                actor: "service-test".to_string(),
                initial_intent: None,
            })
            .expect("create the episode");

        let first = workspace
            .service
            .ensure_episode_cover(&project_id, "service-test", false)
            .expect("draw the cover");
        let manifest: VideoProjectManifest =
            serde_json::from_value(first.get("manifest").cloned().expect("manifest"))
                .expect("decode manifest");
        let card = manifest
            .visual_assets
            .iter()
            .find(|asset| asset.id.starts_with("cover-"))
            .expect("a card was drawn");
        // Recorded as generated, so it is never mistaken for artwork the user supplied.
        assert!(matches!(
            card.provenance.kind,
            ProvenanceKind::GeneratedLocally
        ));
        let card_path = workspace
            .service
            .resolve_managed_path(&card.managed_path)
            .expect("resolve the card");
        assert!(card_path.is_file());
        let revision_after_first = value_i64(&first, "revision").expect("revision");

        // Asking again for an unchanged episode must not churn the revision: the file would be
        // byte-identical, and bumping it would invalidate every render that depends on it.
        let second = workspace
            .service
            .ensure_episode_cover(&project_id, "service-test", false)
            .expect("second request");
        assert_eq!(
            value_i64(&second, "revision").expect("revision"),
            revision_after_first
        );

        // A manifest entry pointing at a file that is gone is not a picture, so it is drawn again
        // rather than reported as present.
        fs::remove_file(&card_path).expect("remove the card");
        workspace
            .service
            .ensure_episode_cover(&project_id, "service-test", false)
            .expect("redraw after the file went missing");
        assert!(card_path.is_file(), "a missing card was not drawn again");

        // An explicit redraw draws even when the card is present and unchanged.
        fs::remove_file(&card_path).expect("remove the card again");
        workspace
            .service
            .ensure_episode_cover(&project_id, "service-test", true)
            .expect("explicit redraw");
        assert!(card_path.is_file());
    }

    #[test]
    fn a_show_format_is_durable_and_an_episode_inherits_it_by_copy() {
        let workspace = TestWorkspace::new();

        let saved = workspace
            .service
            .save_show_format(show_format())
            .expect("save the show format");
        // The service owns the revision so two formats cannot claim the same provenance.
        assert_eq!(saved.revision, 1);
        assert_eq!(workspace.service.list_show_formats().unwrap().len(), 1);

        let episode = workspace
            .service
            .create_episode(&saved.id, "Episode 1", "service-test", None)
            .expect("start the first episode");
        let manifest: VideoProjectManifest =
            serde_json::from_value(episode.get("manifest").cloned().expect("manifest"))
                .expect("decode episode manifest");
        assert_eq!(manifest.cast.len(), 2);
        assert_eq!(manifest.audio_mix.target_lufs_milli, -16_000);
        assert_eq!(
            manifest
                .format_origin
                .as_ref()
                .map(|origin| origin.format_revision),
            Some(1)
        );

        // Recasting the show must not reach the episode that already exists.
        let mut revised = saved.clone();
        revised.cast[1].voice_id = "af-nova".to_string();
        let revised = workspace
            .service
            .save_show_format(revised)
            .expect("revise the show format");
        assert_eq!(revised.revision, 2);
        assert_eq!(revised.created_at, saved.created_at);

        let reloaded = workspace.service.get_project(&manifest.project_id).unwrap();
        let reloaded: VideoProjectManifest =
            serde_json::from_value(reloaded.get("manifest").cloned().expect("manifest"))
                .expect("decode reloaded manifest");
        assert_eq!(reloaded.cast[1].voice_id, "af-bella");

        // A new episode picks up the change.
        let next = workspace
            .service
            .create_episode(&revised.id, "Episode 2", "service-test", None)
            .expect("start the second episode");
        let next: VideoProjectManifest =
            serde_json::from_value(next.get("manifest").cloned().expect("manifest"))
                .expect("decode second episode manifest");
        assert_eq!(next.cast[1].voice_id, "af-nova");
        assert_eq!(
            next.format_origin.map(|origin| origin.format_revision),
            Some(2)
        );

        workspace
            .service
            .delete_show_format(&saved.id)
            .expect("delete the show format");
        assert!(workspace.service.list_show_formats().unwrap().is_empty());
        assert!(workspace
            .service
            .create_episode(&saved.id, "Episode 3", "service-test", None)
            .is_err());
    }

    #[test]
    fn applying_a_script_is_version_bound_idempotent_and_reuses_unchanged_turns() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Story episode");
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();

        let script = "NARRATOR: The harmattan came early.\n\nADAEZE: (quiet) You said you would come back.\n\nNARRATOR: She did not answer.\n";
        let request = VideoScriptRequest {
            project_id: project_id.clone(),
            expected_revision: 1,
            base_version_id: version_id,
            operation_id: "script-draft-1".to_string(),
            cast: script_cast(),
            accept_dropped_cues: false,
            script: script.to_string(),
        };

        let applied = workspace
            .service
            .apply_script(request.clone())
            .expect("commit the first script");
        assert!(!applied.replayed);
        assert_eq!(
            applied.project.get("revision").and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(applied.receipt.new_turn_ids.len(), 3);
        assert!(applied.receipt.retained_turn_ids.is_empty());
        assert_eq!(
            applied
                .project
                .pointer("/manifest/dialogue/1/character_id")
                .and_then(Value::as_str),
            Some("adaeze")
        );
        assert_eq!(
            workspace
                .service
                .store
                .get_job(&applied.job_id)
                .expect("script job")
                .and_then(|job| job.get("status").cloned())
                .and_then(|status| status.as_str().map(str::to_string))
                .as_deref(),
            Some("completed")
        );

        // An exact replay adopts the committed revision instead of writing the script twice.
        let replay = workspace
            .service
            .apply_script(request.clone())
            .expect("adopt the exact script replay");
        assert!(replay.replayed);
        assert_eq!(replay.job_id, applied.job_id);
        assert_eq!(
            replay.project.get("revision").and_then(Value::as_i64),
            Some(2),
            "an idempotent replay must not create another version"
        );
        assert!(
            replay.receipt.new_turn_ids.is_empty(),
            "a replay must never ask the caller to re-render an already committed turn"
        );

        // Rewriting one line must reuse the two untouched turns.
        let next_version = applied
            .project
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("committed version")
            .to_string();
        let revised = workspace
            .service
            .apply_script(VideoScriptRequest {
                project_id: project_id.clone(),
                expected_revision: 2,
                base_version_id: next_version,
                operation_id: "script-draft-2".to_string(),
                cast: script_cast(),
                script: script.replace("She did not answer.", "She said nothing at all."),
                accept_dropped_cues: false,
            })
            .expect("commit the revised script");
        assert_eq!(revised.receipt.new_turn_ids.len(), 1);
        assert_eq!(revised.receipt.retained_turn_ids.len(), 2);
        assert_eq!(
            revised.receipt.retained_turn_ids,
            applied.receipt.new_turn_ids[..2].to_vec()
        );

        // A stale base version cannot silently overwrite the newer revision.
        let stale = workspace
            .service
            .apply_script(VideoScriptRequest {
                operation_id: "script-draft-3".to_string(),
                ..request.clone()
            })
            .expect_err("a stale revision must be rejected");
        assert!(
            stale.code.starts_with("video."),
            "unexpected stale-write code: {}",
            stale.code
        );

        // The same operation id cannot be reused for different content.
        let conflicting = workspace
            .service
            .apply_script(VideoScriptRequest {
                script: "NARRATOR: Something else entirely.\n".to_string(),
                ..request
            })
            .expect_err("operation identifiers cannot be reused with a different script");
        assert_eq!(conflicting.code, "video.idempotency_conflict");
    }

    #[test]
    fn generated_visual_is_managed_versioned_idempotent_and_renderable() {
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        if !ffmpeg.is_file() {
            return;
        }
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Generated visual contract");
        let source = workspace.root.join("generated-illustration.png");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x5C7CFA:s=320x180:d=0.04",
                "-frames:v",
                "1",
                "-threads",
                "1",
            ])
            .arg(&source)
            .status()
            .expect("generate illustration");
        assert!(status.success());
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();
        let generation_receipt = workspace
            .service
            .register_trusted_generated_visual(
                AuthorizeVisualSelectionRequest {
                    project_id: project_id.clone(),
                    expected_revision: 1,
                    expected_version_id: version_id.clone(),
                },
                TrustedGeneratedVisual {
                    thread_id: "thread-generated-visual".to_string(),
                    turn_id: "turn-generated-visual".to_string(),
                    generation_id: "image-generation-one".to_string(),
                    source_path: source.clone(),
                    producer_version: Some("test-broker-1".to_string()),
                    revised_prompt: Some("A blue editorial illustration".to_string()),
                },
            )
            .expect("register authenticated generation");
        assert_eq!(generation_receipt.receipt_kind, "generated_locally");
        let request = AddVisualAssetRequest {
            project_id: project_id.clone(),
            expected_revision: 1,
            expected_version_id: version_id,
            operation_id: "generated-illustration-one".to_string(),
            actor: "codex-video-agent".to_string(),
            origin: VisualAssetOrigin::GeneratedLocally {
                receipt_id: generation_receipt.id,
            },
            scene_id: None,
            range: TimeRange::new(0, 1_000_000).expect("visual range"),
            fit: VisualFit::Contain,
            crop: None,
            z_index: 1,
            motion: VisualMotion {
                start_bounds: NormalizedRect {
                    x_bp: 1_000,
                    y_bp: 2_000,
                    width_bp: 8_000,
                    height_bp: 4_500,
                },
                end_bounds: NormalizedRect {
                    x_bp: 200,
                    y_bp: 1_500,
                    width_bp: 9_600,
                    height_bp: 5_400,
                },
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: super::super::VisualEasing::EaseInOut,
            },
            transition_in_us: Microseconds(100_000),
            transition_out_us: Microseconds(100_000),
        };
        let added = workspace
            .service
            .add_visual_asset(request.clone())
            .expect("add generated visual");
        assert!(!added.replayed);
        assert_eq!(
            added.project.get("revision").and_then(Value::as_i64),
            Some(2)
        );
        let manifest: VideoProjectManifest = serde_json::from_value(
            added
                .project
                .get("manifest")
                .cloned()
                .expect("visual manifest"),
        )
        .expect("typed visual manifest");
        assert_eq!(manifest.visual_assets.len(), 1);
        assert_eq!(manifest.visual_layers.len(), 1);
        let managed = workspace
            .service
            .resolve_managed_path(&manifest.visual_assets[0].managed_path)
            .expect("managed visual");
        assert!(managed.is_file());
        assert_eq!(fs::metadata(&managed).expect("visual metadata").nlink(), 1);
        assert_eq!(
            manifest.visual_assets[0]
                .provenance
                .metadata
                .get("generation_id"),
            Some(&Value::String("image-generation-one".to_string()))
        );
        let replay = workspace
            .service
            .add_visual_asset(request)
            .expect("adopt generated visual replay");
        assert!(replay.replayed);
        assert_eq!(replay.asset_id, added.asset_id);
        assert_eq!(replay.layer_id, added.layer_id);
        assert_eq!(
            replay.project.get("revision").and_then(Value::as_i64),
            Some(2)
        );
    }

    #[test]
    fn headless_broker_registration_adds_visual_without_desktop_state() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Headless generated visual");
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();
        let source = workspace.root.join("headless-generated.png");
        fs::write(&source, test_png_bytes(96, 54)).expect("write headless generation output");
        let hostile_source = workspace.root.join("hostile-generated.png");
        fs::write(&hostile_source, test_png_bytes(12, 12)).expect("write hostile source");

        let broker = workspace.root.join("fake-codex-broker");
        let thread_response = json!({
            "id": 2,
            "result": {
                "thread": {
                    "id": "thread-headless",
                    "turns": [{
                        "id": "turn-headless",
                        "items": [{
                            "type": "imageGeneration",
                            "id": "generation-headless",
                            "status": "completed",
                            "savedPath": source,
                            "revisedPrompt": "A headless generated illustration",
                            "failure": null
                        }]
                    }]
                }
            }
        })
        .to_string();
        let c_response = thread_response.replace('\\', "\\\\").replace('"', "\\\"");
        let mut broker_source = String::from(
            "#include <stdio.h>\n#include <string.h>\nstatic const char response[] = \"",
        );
        broker_source.push_str(&c_response);
        broker_source.push_str(
            "\";\nint main(void) { char line[65536]; while (fgets(line, sizeof(line), stdin)) { if (strstr(line, \"\\\"id\\\":1\")) puts(\"{\\\"id\\\":1,\\\"result\\\":{}}\"); else if (strstr(line, \"\\\"id\\\":2\")) puts(response); fflush(stdout); } return 0; }\n",
        );
        let broker_c = workspace.root.join("fake-codex-broker.c");
        fs::write(&broker_c, broker_source).expect("write fake broker source");
        let compiler = ["/usr/bin/cc", "/bin/cc"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("C compiler for ELF broker regression");
        let compile = Command::new(compiler)
            .arg(&broker_c)
            .arg("-O2")
            .arg("-o")
            .arg(&broker)
            .status()
            .expect("compile fake ELF broker");
        assert!(compile.success());

        let account_home = crate::codex_agent::account_home_dir_for_test();
        let broker_test_root = account_home
            .join(".config/soundar")
            .join(format!("headless-broker-test-{}", Uuid::new_v4().simple()));
        let codex_home = broker_test_root.join("codex-home");
        let mut codex_home_builder = fs::DirBuilder::new();
        codex_home_builder.recursive(true).mode(0o700);
        codex_home_builder
            .create(&codex_home)
            .expect("create private test Codex home");
        fs::set_permissions(&codex_home, fs::Permissions::from_mode(0o700))
            .expect("secure test Codex home");
        let identity_path = workspace
            .root
            .join("broker-identity/trusted-codex-generation-broker-v1.json");
        crate::codex_agent::enroll_test_codex_broker_at(&identity_path, &broker, &codex_home)
            .expect("enroll pinned ELF broker");

        let hostile_shim = workspace.root.join("codex");
        fs::write(
            &hostile_shim,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 999.0.0'; exit 0; fi\nprintf '%s\\n' '{}'\n",
                json!({
                    "id": 2,
                    "result": {"thread": {"id": "thread-headless", "turns": [{"id":"hostile-turn", "items":[{
                        "type":"imageGeneration", "id":"generation-headless", "status":"completed", "savedPath": hostile_source, "failure": null
                    }]}]}}
                })
            ),
        )
        .expect("write hostile Codex shim");
        fs::set_permissions(&hostile_shim, fs::Permissions::from_mode(0o700))
            .expect("make hostile shim executable");
        let old_soundar_codex = std::env::var_os("SOUNDAR_CODEX_BIN");
        let old_codex = std::env::var_os("CODEX_BIN");
        let old_path = std::env::var_os("PATH");
        std::env::set_var("SOUNDAR_CODEX_BIN", &hostile_shim);
        std::env::set_var("CODEX_BIN", &hostile_shim);
        std::env::set_var("PATH", &workspace.root);
        let missing_identity_error = crate::codex_agent::resolve_headless_generated_visual_at(
            &identity_path.with_file_name("missing-broker-identity.json"),
            "thread-headless",
            "generation-headless",
        )
        .expect_err("hostile discovery variables cannot bootstrap a missing enrollment");
        assert_eq!(
            missing_identity_error.code,
            "video.generation_broker_setup_required"
        );
        let generation_result = crate::codex_agent::resolve_headless_generated_visual_at(
            &identity_path,
            "thread-headless",
            "generation-headless",
        );
        match old_soundar_codex {
            Some(value) => std::env::set_var("SOUNDAR_CODEX_BIN", value),
            None => std::env::remove_var("SOUNDAR_CODEX_BIN"),
        }
        match old_codex {
            Some(value) => std::env::set_var("CODEX_BIN", value),
            None => std::env::remove_var("CODEX_BIN"),
        }
        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        let generation = generation_result.expect("resolve only through pinned headless broker");
        assert_eq!(generation.source_path, source);
        let receipt = workspace
            .service
            .register_trusted_generated_visual(
                AuthorizeVisualSelectionRequest {
                    project_id: project_id.clone(),
                    expected_revision: 1,
                    expected_version_id: version_id.clone(),
                },
                generation,
            )
            .expect("register headless generation");
        let added = workspace
            .service
            .add_visual_asset(AddVisualAssetRequest {
                project_id,
                expected_revision: 1,
                expected_version_id: version_id,
                operation_id: "headless-generated-visual".to_string(),
                actor: "codex-video-agent-headless".to_string(),
                origin: VisualAssetOrigin::GeneratedLocally {
                    receipt_id: receipt.id,
                },
                scene_id: None,
                range: TimeRange::new(0, 1_000_000).expect("visual range"),
                fit: VisualFit::Contain,
                crop: None,
                z_index: 1,
                motion: test_visual_motion(),
                transition_in_us: Microseconds(0),
                transition_out_us: Microseconds(0),
            })
            .expect("add headless registered generation");
        assert_eq!(added.project["revision"], 2);
        assert_eq!(
            added.project["manifest"]["visual_assets"][0]["provenance"]["metadata"]
                ["generation_id"],
            "generation-headless"
        );
        let moved_broker = workspace.root.join("moved-fake-codex-broker");
        fs::rename(&broker, &moved_broker).expect("move enrolled broker executable");
        std::os::unix::fs::symlink(&hostile_shim, &broker)
            .expect("replace enrolled broker path with hostile shim");
        let changed = crate::codex_agent::resolve_headless_generated_visual_at(
            &identity_path,
            "thread-headless",
            "generation-headless",
        )
        .expect_err("an enrolled broker path replacement must fail closed");
        assert_eq!(changed.code, "video.generation_broker_identity_changed");
        fs::remove_dir_all(broker_test_root).ok();
    }

    #[test]
    fn native_selection_receipt_binds_exact_file_and_persists_authorization() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Native visual receipt");
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();
        let source = workspace.root.join("picked-image.png");
        fs::write(&source, test_png_bytes(64, 32)).expect("write selected image");
        let receipt = workspace
            .service
            .authorize_user_visual_selection(
                AuthorizeVisualSelectionRequest {
                    project_id: project_id.clone(),
                    expected_revision: 1,
                    expected_version_id: version_id.clone(),
                },
                source,
            )
            .expect("mint native picker receipt");
        assert_eq!(receipt.receipt_kind, "user_selected");
        let added = workspace
            .service
            .add_visual_asset(AddVisualAssetRequest {
                project_id,
                expected_revision: 1,
                expected_version_id: version_id,
                operation_id: "native-picked-image".to_string(),
                actor: "local-user".to_string(),
                origin: VisualAssetOrigin::UserSelected {
                    receipt_id: receipt.id.clone(),
                },
                scene_id: None,
                range: TimeRange::new(0, 1_000_000).expect("visual range"),
                fit: VisualFit::Cover,
                crop: None,
                z_index: 1,
                motion: test_visual_motion(),
                transition_in_us: Microseconds(0),
                transition_out_us: Microseconds(0),
            })
            .expect("add picker-authorized visual");
        assert_eq!(added.project["revision"], 2);
        assert_eq!(
            added.project["manifest"]["visual_assets"][0]["provenance"]["metadata"]
                ["authorization_receipt_id"],
            receipt.id
        );
        assert_eq!(
            added.project["manifest"]["visual_assets"][0]["provenance"]["producer"],
            "soundAr native file picker"
        );
    }

    #[test]
    fn visual_receipt_rejects_source_replacement_and_unsafe_existing_target() {
        for attack in ["source_symlink", "managed_hardlink"] {
            let workspace = TestWorkspace::new();
            let project_id = format!("project-{}", new_id());
            let created = workspace.create_project(&project_id, "Visual receipt attacks");
            let version_id = created
                .pointer("/version/id")
                .and_then(Value::as_str)
                .expect("initial version")
                .to_string();
            let source = workspace.root.join(format!("picked-{attack}.png"));
            let alternate = workspace.root.join(format!("alternate-{attack}.png"));
            fs::write(&source, test_png_bytes(48, 48)).expect("write selected source");
            fs::write(&alternate, test_png_bytes(48, 48)).expect("write alternate source");
            let receipt = workspace
                .service
                .authorize_user_visual_selection(
                    AuthorizeVisualSelectionRequest {
                        project_id: project_id.clone(),
                        expected_revision: 1,
                        expected_version_id: version_id.clone(),
                    },
                    source.clone(),
                )
                .expect("mint exact source receipt");
            let request = AddVisualAssetRequest {
                project_id: project_id.clone(),
                expected_revision: 1,
                expected_version_id: version_id,
                operation_id: format!("visual-attack-{attack}"),
                actor: "local-user".to_string(),
                origin: VisualAssetOrigin::UserSelected {
                    receipt_id: receipt.id,
                },
                scene_id: None,
                range: TimeRange::new(0, 1_000_000).expect("visual range"),
                fit: VisualFit::Cover,
                crop: None,
                z_index: 1,
                motion: test_visual_motion(),
                transition_in_us: Microseconds(0),
                transition_out_us: Microseconds(0),
            };
            if attack == "source_symlink" {
                fs::remove_file(&source).expect("replace selected path");
                std::os::unix::fs::symlink(&alternate, &source).expect("install source symlink");
                let error = workspace
                    .service
                    .add_visual_asset(request)
                    .expect_err("source replacement must fail closed");
                assert!(
                    matches!(
                        error.code.as_str(),
                        "video.visual_not_found" | "video.visual_receipt_mismatch"
                    ),
                    "unexpected source replacement error: {error:?}"
                );
                continue;
            }

            let request_value = serde_json::to_value(&request).expect("serialize attack request");
            let idempotency_key = format!(
                "visual-add:{}",
                sha256_bytes(format!("{}:{}", request.project_id, request.operation_id).as_bytes())
            );
            let (job_id, created_job) = workspace
                .service
                .store
                .create_idempotent_job("video_add_visual_asset", &idempotency_key, &request_value)
                .expect("create attack import job")
                .expect("stable attack job");
            assert!(created_job);
            workspace
                .service
                .store
                .fail_job(&job_id, "test failpoint")
                .expect("make attack job resumable");
            let asset_id = stable_import_asset_id(
                &request.project_id,
                &job_id,
                "visual",
                &request.operation_id,
            );
            let visual_dir = workspace
                .service
                .project_dir(&request.project_id)
                .expect("project directory")
                .join("visuals");
            workspace
                .service
                .secure_managed_directory(&visual_dir)
                .expect("visual directory");
            let final_path = visual_dir.join(format!("{asset_id}.png"));
            fs::hard_link(&alternate, &final_path).expect("install managed hard-link alias");
            let error = workspace
                .service
                .add_visual_asset(request)
                .expect_err("pre-existing hard-link target must fail closed");
            assert_eq!(error.code, "video.unsafe_artifact_path");
        }
    }

    #[test]
    fn generated_receipt_rejects_a_symlink_at_its_registered_path() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Generated receipt path attack");
        let version_id = created
            .pointer("/version/id")
            .and_then(Value::as_str)
            .expect("initial version")
            .to_string();
        let source = workspace.root.join("generated-path-attack.png");
        fs::write(&source, test_png_bytes(48, 48)).expect("write generation output");
        let receipt = workspace
            .service
            .register_trusted_generated_visual(
                AuthorizeVisualSelectionRequest {
                    project_id: project_id.clone(),
                    expected_revision: 1,
                    expected_version_id: version_id.clone(),
                },
                TrustedGeneratedVisual {
                    thread_id: "thread-path-attack".to_string(),
                    turn_id: "turn-path-attack".to_string(),
                    generation_id: "image-path-attack".to_string(),
                    source_path: source.clone(),
                    producer_version: Some("test-broker-1".to_string()),
                    revised_prompt: None,
                },
            )
            .expect("register generated path");
        let stored = workspace
            .service
            .store
            .get_video_visual_source_receipt(&receipt.id)
            .expect("read generated receipt")
            .expect("stored generated receipt");
        let registered_path = PathBuf::from(
            stored["source_path"]
                .as_str()
                .expect("registered source path"),
        );
        let moved_path = registered_path.with_extension("moved.png");
        fs::rename(&registered_path, &moved_path).expect("move registered bytes");
        fs::copy(&moved_path, &registered_path).expect("replace with byte-identical new inode");
        let replay_error = workspace
            .service
            .register_trusted_generated_visual(
                AuthorizeVisualSelectionRequest {
                    project_id: project_id.clone(),
                    expected_revision: 1,
                    expected_version_id: version_id.clone(),
                },
                TrustedGeneratedVisual {
                    thread_id: "thread-path-attack".to_string(),
                    turn_id: "turn-path-attack".to_string(),
                    generation_id: "image-path-attack".to_string(),
                    source_path: source,
                    producer_version: Some("test-broker-1".to_string()),
                    revised_prompt: None,
                },
            )
            .expect_err("registration replay must retain exact managed file identity");
        assert_eq!(replay_error.code, "video.generation_identity_conflict");
        fs::remove_file(&registered_path).expect("remove byte-identical replacement");
        std::os::unix::fs::symlink(&moved_path, &registered_path)
            .expect("replace registered path with symlink");

        let error = workspace
            .service
            .add_visual_asset(AddVisualAssetRequest {
                project_id,
                expected_revision: 1,
                expected_version_id: version_id,
                operation_id: "generated-path-symlink".to_string(),
                actor: "codex-video-agent".to_string(),
                origin: VisualAssetOrigin::GeneratedLocally {
                    receipt_id: receipt.id,
                },
                scene_id: None,
                range: TimeRange::new(0, 1_000_000).expect("visual range"),
                fit: VisualFit::Cover,
                crop: None,
                z_index: 1,
                motion: test_visual_motion(),
                transition_in_us: Microseconds(0),
                transition_out_us: Microseconds(0),
            })
            .expect_err("registered generated symlink must fail closed");
        assert_eq!(error.code, "video.unsafe_artifact_path");
    }

    #[test]
    fn revision_rejects_history_rewrites_and_declared_diff_mismatches() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Immutable revision contract");
        let original: VideoProjectManifest =
            serde_json::from_value(created.get("manifest").cloned().expect("manifest"))
                .expect("typed manifest");

        let make_revision = |mut manifest: VideoProjectManifest, declared_path: &str| {
            manifest.name = "Changed name".to_string();
            manifest.revision = 2;
            let parent_id = manifest
                .revision_history
                .last()
                .map(|record| record.id.clone());
            manifest.revision_history.push(RevisionRecord {
                id: new_id(),
                revision: 2,
                parent_id,
                actor: "service-test".to_string(),
                reason: "Adversarial edit".to_string(),
                changed_paths: vec![declared_path.to_string()],
                invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
                created_at: utc_now(),
            });
            manifest.updated_at = utc_now();
            manifest
        };

        let mut rewritten = original.clone();
        rewritten.revision_history[0].reason = "Rewritten history".to_string();
        let rewritten = make_revision(rewritten, "/name");
        let error = workspace
            .service
            .revise_manifest(ReviseVideoManifestRequest {
                project_id: project_id.clone(),
                expected_revision: 1,
                manifest: rewritten,
                actor: "service-test".to_string(),
                reason: "Adversarial edit".to_string(),
                changed_paths: vec!["/name".to_string()],
                invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
                status: None,
            })
            .expect_err("prior history cannot be rewritten");
        assert_eq!(error.code, "video.revision_history_modified");

        let mismatched = make_revision(original, "/layout");
        let error = workspace
            .service
            .revise_manifest(ReviseVideoManifestRequest {
                project_id,
                expected_revision: 1,
                manifest: mismatched,
                actor: "service-test".to_string(),
                reason: "Adversarial edit".to_string(),
                changed_paths: vec!["/layout".to_string()],
                invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
                status: None,
            })
            .expect_err("declared paths cannot conceal the actual diff");
        assert_eq!(error.code, "video.revision_diff_mismatch");
    }

    #[test]
    fn rights_receipt_timestamp_is_normalized_to_contract_utc() {
        assert_eq!(
            normalize_utc_timestamp("2026-08-27T12:34:56+00:00").expect("valid timestamp"),
            "2026-08-27T12:34:56.000Z"
        );
    }

    #[test]
    fn managed_path_components_reject_dot_traversal_names() {
        for value in [".", "..", "../project", "project/..", "project\\.."] {
            assert!(
                validate_safe_component(value, "video.invalid_project_id").is_err(),
                "unsafe managed component {value:?} must be rejected"
            );
        }
        for value in ["project-01", "cache.v2", "safe_namespace"] {
            validate_safe_component(value, "video.invalid_project_id")
                .expect("ordinary safe component");
        }
    }

    #[test]
    fn deterministic_import_targets_reject_changed_local_and_link_extensions() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Deterministic import publication");
        let source_dir = workspace
            .service
            .project_dir(&project_id)
            .expect("project directory")
            .join("sources");
        workspace
            .service
            .secure_managed_directory(&source_dir)
            .expect("source directory");

        for (role, original_extension, changed_extension) in [
            ("local-primary", "wav", "mp3"),
            ("link-primary", "webm", "mp4"),
        ] {
            let asset_id = stable_import_asset_id(&project_id, "durable-child", "source", role);
            let original_bytes = format!("original-{role}-bytes").into_bytes();
            let original_sha256 = sha256_bytes(&original_bytes);
            let first_staging = source_dir.join(format!(".{asset_id}.first.partial"));
            fs::write(&first_staging, &original_bytes).expect("stage original import bytes");
            let original_target =
                source_dir.join(format!("source-{asset_id}.{original_extension}"));
            let (published, created) = workspace
                .service
                .publish_import_staging_once(
                    &project_id,
                    &asset_id,
                    &first_staging,
                    &original_target,
                    &original_sha256,
                    &AtomicBool::new(false),
                    |_| Ok(()),
                )
                .expect("first deterministic publication");
            assert!(created);
            assert_eq!(published, original_target);

            let changed_bytes = format!("changed-{role}-bytes").into_bytes();
            let changed_sha256 = sha256_bytes(&changed_bytes);
            let retry_staging = source_dir.join(format!(".{asset_id}.retry.partial"));
            fs::write(&retry_staging, changed_bytes).expect("stage changed retry bytes");
            let changed_target = source_dir.join(format!("source-{asset_id}.{changed_extension}"));
            let error = workspace
                .service
                .publish_import_staging_once(
                    &project_id,
                    &asset_id,
                    &retry_staging,
                    &changed_target,
                    &changed_sha256,
                    &AtomicBool::new(false),
                    |_| Ok(()),
                )
                .expect_err("changed durable import bytes must not replace or fork the target");
            assert_eq!(error.code, "video.import_identity_conflict");
            assert_eq!(
                sha256_file(&original_target).expect("original target checksum"),
                original_sha256
            );
            assert!(!changed_target.exists());
            assert!(!retry_staging.exists());
            let stable_targets = fs::read_dir(&source_dir)
                .expect("list stable targets")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(&format!("source-{asset_id}.")))
                })
                .count();
            assert_eq!(stable_targets, 1, "one stable asset id owns one inode");
        }
    }

    #[test]
    fn shared_gpu_gate_serializes_video_work_and_releases_every_lease() {
        let state = Arc::new(RecordingGpuState::default());
        let gate = Arc::new(RecordingGpuGate {
            state: Arc::clone(&state),
        });
        let shared_gate: Arc<dyn SharedGpuAdmissionGate> = gate;
        let workspace = TestWorkspace::with_gpu_gate(shared_gate);
        let first_cancel = AtomicBool::new(false);
        let first = workspace
            .service
            .acquire_resources(
                "gpu-video-one",
                "project-gpu-gate",
                ResourceRequest::medium_nvenc(),
                &first_cancel,
                None,
            )
            .expect("first shared GPU lease");
        assert_eq!(state.active.load(Ordering::Acquire), 1);

        let (acquired_sender, acquired_receiver) = std::sync::mpsc::channel();
        let second_service = Arc::clone(&workspace.service);
        let second = thread::spawn(move || -> ServiceResult<()> {
            let cancel = AtomicBool::new(false);
            let lease = second_service.acquire_resources(
                "gpu-video-two",
                "project-gpu-gate",
                ResourceRequest::medium_nvenc(),
                &cancel,
                None,
            )?;
            acquired_sender.send(()).expect("announce second admission");
            drop(lease);
            Ok(())
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.attempts.load(Ordering::Acquire) < 2 {
            assert!(
                Instant::now() < deadline,
                "second request did not reach gate"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            acquired_receiver
                .recv_timeout(Duration::from_millis(75))
                .is_err(),
            "the fake global gate must serialize overlapping GPU work"
        );
        drop(first);
        acquired_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("second request admitted after RAII release");
        second
            .join()
            .expect("second GPU worker joins")
            .expect("second GPU work");
        assert_eq!(state.active.load(Ordering::Acquire), 0);
        assert_eq!(state.maximum_active.load(Ordering::Acquire), 1);
        assert_eq!(state.releases.load(Ordering::Acquire), 2);

        let requests = state.requests.lock().expect("recorded requests");
        let first_request = requests
            .iter()
            .find(|request| request.get("job_id").and_then(Value::as_str) == Some("gpu-video-one"))
            .expect("serialized first request");
        assert_eq!(first_request.get("resource_class"), Some(&json!("medium")));
        assert_eq!(first_request.get("requested_vram_mb"), Some(&json!(1_024)));
        assert_eq!(
            first_request.get("requested_nvenc_sessions"),
            Some(&json!(1))
        );
        assert_eq!(first_request.get("exclusive"), Some(&json!(false)));
        drop(requests);

        let attempts_before_cpu = state.attempts.load(Ordering::Acquire);
        let cpu_cancel = AtomicBool::new(false);
        let cpu_lease = workspace
            .service
            .acquire_resources(
                "cpu-video-only",
                "project-gpu-gate",
                ResourceRequest::light(),
                &cpu_cancel,
                None,
            )
            .expect("CPU-only work keeps local admission");
        drop(cpu_lease);
        assert_eq!(
            state.attempts.load(Ordering::Acquire),
            attempts_before_cpu,
            "CPU-only work must not enter the shared GPU gate"
        );
    }

    #[test]
    fn shared_gpu_wait_is_cancellable_without_leaking_local_capacity() {
        let state = Arc::new(RecordingGpuState::default());
        state.blocked.store(true, Ordering::Release);
        let gate = Arc::new(RecordingGpuGate {
            state: Arc::clone(&state),
        });
        let shared_gate: Arc<dyn SharedGpuAdmissionGate> = gate;
        let workspace = TestWorkspace::with_gpu_gate(shared_gate);
        let service = Arc::clone(&workspace.service);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker = thread::spawn(move || {
            service
                .acquire_resources(
                    "gpu-video-cancel",
                    "project-gpu-cancel",
                    ResourceRequest::medium_nvenc(),
                    &worker_cancel,
                    None,
                )
                .map(drop)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while state.attempts.load(Ordering::Acquire) == 0 {
            assert!(
                Instant::now() < deadline,
                "blocked request did not reach gate"
            );
            thread::sleep(Duration::from_millis(5));
        }
        cancel.store(true, Ordering::Release);
        let error = worker
            .join()
            .expect("cancelled GPU worker joins")
            .expect_err("waiting GPU admission must observe cancellation");
        assert_eq!(error.code, "video.cancelled");
        assert_eq!(state.active.load(Ordering::Acquire), 0);
        assert_eq!(state.releases.load(Ordering::Acquire), 0);
        let local_usage = workspace
            .service
            .scheduler
            .lock()
            .expect("local scheduler")
            .usage();
        assert_eq!(local_usage.total_jobs(), 0);
        assert_eq!(local_usage.vram_mb, 0);
        assert_eq!(local_usage.nvenc_sessions, 0);
    }

    #[test]
    fn publish_manifest_redacts_secret_query_values_but_keeps_exact_url_evidence() {
        let exact = "https://media.example.com/source.mp4?token=do-not-export&expires=9999";
        let exact_sha = sha256_bytes(exact.as_bytes());
        let mut manifest = empty_manifest("project-private-link", "Private link");
        manifest.rights_confirmations.push(RightsConfirmation {
            id: "rights-private-link".to_string(),
            source_uri: exact.to_string(),
            source_uri_sha256: exact_sha.clone(),
            basis: RightsBasis::Owned,
            confirmation_text: "I own this source".to_string(),
            confirmed_by: "service-test".to_string(),
            confirmed_at: utc_now(),
            single_source_only: true,
        });

        let (published, redacted) = redact_publish_manifest_urls(&manifest);
        assert!(redacted);
        assert_eq!(
            published.rights_confirmations[0].source_uri,
            "https://media.example.com/source.mp4"
        );
        assert_eq!(
            published.rights_confirmations[0].source_uri_sha256, exact_sha,
            "the exact authorized URL remains bound without disclosing its secret query"
        );
        let serialized = serde_json::to_string(&published).expect("published manifest JSON");
        assert!(!serialized.contains("do-not-export"));
        assert!(!serialized.contains("expires=9999"));

        assert_eq!(
            publish_safe_source_uri("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
                false
            ),
            "the canonical public YouTube video identifier remains useful"
        );
    }

    #[test]
    fn exact_link_rights_are_checked_before_runtime_admission() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Rights contract");
        let error = workspace
            .service
            .queue_link_import(
                LinkImportRequest {
                    project_id,
                    url: "https://youtu.be/dQw4w9WgXcQ".to_string(),
                    actor: "service-test".to_string(),
                    rights: LinkRightsRequest {
                        confirmed_url: "https://www.youtube.com/watch?v=another".to_string(),
                        basis: RightsBasis::Owned,
                        statement: "I own this exact source".to_string(),
                        confirmed_by: "service-test".to_string(),
                    },
                    title: None,
                },
                None,
            )
            .expect_err("mismatched exact URL must fail");
        assert_eq!(error.code, "video.rights_url_mismatch");
    }

    #[test]
    fn link_download_args_omit_exit_101_sentinel_and_keep_single_source_guards() {
        let output_template = Path::new("/managed/.download-fixture.%(ext)s");
        let canonical_url = "https://example.com/one-authorized-source.webm";
        let proxy_url = "http://127.0.0.1:43117/auth-token";
        let args = yt_dlp_single_source_download_args(output_template, canonical_url, proxy_url);
        assert!(args.iter().any(|arg| arg == "--no-playlist"));
        assert!(args.iter().any(|arg| arg == "--playlist-items"));
        assert!(args.iter().any(|arg| arg == "--ignore-config"));
        assert!(args.iter().any(|arg| arg == "--max-filesize"));
        assert!(args.iter().any(|arg| arg == "--match-filter"));
        assert!(args.iter().any(|arg| arg == "--proxy"));
        assert!(args.iter().any(|arg| arg == "--output"));
        assert!(
            !args.iter().any(|arg| arg == "--max-downloads"),
            "yt-dlp turns a successful first download into exit 101 when this sentinel is present"
        );
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("end-of-options separator");
        assert_eq!(
            &args[separator + 1..],
            &[OsString::from(canonical_url)],
            "the invocation has exactly one positional, already validated URL"
        );
        assert_eq!(args.iter().filter(|arg| *arg == "--no-playlist").count(), 1);
        assert_eq!(
            args.iter()
                .position(|arg| arg == "--playlist-items")
                .and_then(|index| args.get(index + 1)),
            Some(&OsString::from("1")),
            "playlist/feed extractors remain capped at one selected entry"
        );
        assert_eq!(
            args.iter().filter(|arg| *arg == "--playlist-items").count(),
            1
        );
        assert_eq!(
            args.iter()
                .position(|arg| arg == "--proxy")
                .and_then(|index| args.get(index + 1)),
            Some(&OsString::from(proxy_url))
        );
        assert_eq!(
            args.iter()
                .position(|arg| arg == "--output")
                .and_then(|index| args.get(index + 1)),
            Some(&output_template.as_os_str().to_os_string())
        );

        let workspace = TestWorkspace::new();
        let directory = workspace.root.join("single-link-output");
        fs::create_dir(&directory).expect("single-link fixture directory");
        let prefix = ".download-fixture";
        let completed = directory.join(format!("{prefix}.webm"));
        fs::write(&completed, b"one completed source").expect("completed source fixture");
        fs::write(directory.join(format!("{prefix}.part")), b"partial")
            .expect("partial source fixture");
        assert_eq!(
            single_downloaded_file(&directory, prefix).expect("one completed source"),
            completed
        );
        fs::write(
            directory.join(format!("{prefix}.mp4")),
            b"second completed source",
        )
        .expect("second source fixture");
        assert_eq!(
            single_downloaded_file(&directory, prefix)
                .expect_err("multiple completed files must fail closed")
                .code,
            "video.single_source_required"
        );
    }

    #[test]
    #[ignore = "production network smoke; set SOUNDAR_RUN_NETWORK_VIDEO_SMOKE=1"]
    fn production_wikimedia_public_domain_link_import_render_and_package() {
        if std::env::var("SOUNDAR_RUN_NETWORK_VIDEO_SMOKE").as_deref() != Ok("1") {
            eprintln!("Set SOUNDAR_RUN_NETWORK_VIDEO_SMOKE=1 to run the Wikimedia link smoke");
            return;
        }
        let workspace = TestWorkspace::new();
        let runtime = workspace.service.runtime_status(true);
        assert!(runtime.ready_for_link_import, "link runtime must be ready");
        assert!(runtime.ready_for_local_media, "media runtime must be ready");
        let source_url =
            "https://commons.wikimedia.org/wiki/File:CC_Public_Domain_Mark_video_bumper.webm";
        let preview = workspace
            .service
            .preview_link(source_url)
            .expect("preview one Wikimedia source through the confined proxy");
        assert!(!preview.canonical_url.is_empty());
        assert!(preview.rights_confirmation_required);
        assert!(preview.has_video);

        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Wikimedia public-domain smoke");
        let import = workspace
            .service
            .queue_link_import(
                LinkImportRequest {
                    project_id: project_id.clone(),
                    url: preview.canonical_url.clone(),
                    actor: "production-link-smoke".to_string(),
                    rights: LinkRightsRequest {
                        confirmed_url: preview.canonical_url,
                        basis: RightsBasis::PublicDomain,
                        statement: "This exact Wikimedia Commons source is marked for public-domain use; import only this one confirmed source.".to_string(),
                        confirmed_by: "soundAr production smoke".to_string(),
                    },
                    title: Some(preview.title),
                },
                None,
            )
            .expect("queue authorized Wikimedia import");
        let imported = workspace
            .service
            .wait_for_job(&import.job_id, &project_id, Duration::from_secs(10 * 60))
            .expect("import authorized Wikimedia source");
        let imported_manifest: VideoProjectManifest =
            serde_json::from_value(imported.project.get("manifest").cloned().expect("manifest"))
                .expect("typed imported manifest");
        assert_eq!(imported_manifest.rights_confirmations.len(), 1);
        assert_eq!(
            imported_manifest.rights_confirmations[0].basis,
            RightsBasis::PublicDomain
        );
        let source = imported_manifest
            .source_assets
            .iter()
            .find(|source| matches!(source.kind, SourceAssetKind::ImportedLink))
            .expect("authorized link source")
            .clone();
        assert!(imported_manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.role == RenderArtifactRole::Proxy));
        let duration_us = source.probe.duration_us.0.min(3_000_000);
        assert!(duration_us > 0);
        let source_id = source.id.clone();
        let reviewed = workspace
            .service
            .commit_manifest_mutation(
                &project_id,
                "production-link-smoke",
                "Review a short public-domain source-clock scene",
                Some("ready"),
                vec![
                    "/timeline_duration_us".to_string(),
                    "/reviewed_scenes".to_string(),
                    "/tracks".to_string(),
                ],
                BTreeSet::from([
                    RevisionStage::Plan,
                    RevisionStage::SceneRender,
                    RevisionStage::Preview,
                    RevisionStage::FinalRender,
                    RevisionStage::PublishPackage,
                ]),
                move |manifest| {
                    manifest.timeline_duration_us = Microseconds(duration_us);
                    manifest.reviewed_scenes = vec![ReviewedScene {
                        id: "wikimedia-scene".to_string(),
                        candidate_id: None,
                        source_asset_id: Some(source_id.clone()),
                        source_range: Some(TimeRange::new(0, duration_us)?),
                        timeline_start_us: Microseconds::ZERO,
                        timeline_duration_us: Microseconds(duration_us),
                        title: "Public-domain mark".to_string(),
                        script: "A short public-domain source verification.".to_string(),
                        review_state: ReviewState::Approved,
                        revision: 1,
                    }];
                    let make_clip = |id: &str| TimelineClip {
                        id: id.to_string(),
                        scene_id: Some("wikimedia-scene".to_string()),
                        turn_id: None,
                        media: MediaReference {
                            source_asset_id: Some(source_id.clone()),
                            render_artifact_id: None,
                        },
                        source_range: TimeRange::new(0, duration_us)
                            .expect("bounded source-clock range"),
                        timeline_start_us: Microseconds::ZERO,
                        timeline_duration_us: Microseconds(duration_us),
                        playback_rate: RationalRate::ONE,
                        gain_db_milli: 0,
                        muted: false,
                        crop: None,
                    };
                    // The canonical assembler carries embedded audio from the
                    // primary video track and synthesizes silence when absent.
                    manifest.tracks = vec![TimelineTrack {
                        id: "wikimedia-video".to_string(),
                        kind: TrackKind::Video,
                        clips: vec![make_clip("wikimedia-video-clip")],
                        preserve_gaps: true,
                    }];
                    manifest.gaps.clear();
                    Ok(())
                },
            )
            .expect("commit reviewed public-domain scene");
        let expectation = project_expectation(&reviewed).expect("reviewed expectation");
        let render = workspace
            .service
            .queue_timeline_render_batch(
                TimelineRenderBatchRequest {
                    base: TimelineRenderRequest {
                        project_id: project_id.clone(),
                        expected_revision: expectation.revision,
                        expected_version_id: expectation.version_id,
                        profile: TimelineRenderProfile::Final,
                        caption_theme: CaptionTheme::Calm,
                        portrait_layout: PortraitSourceLayout::Contain,
                        actor: "production-link-smoke".to_string(),
                        variation: 0,
                        include_title_cards: true,
                        include_speaker_cards: false,
                        burn_captions: true,
                    },
                    variations: vec![0],
                },
                None,
            )
            .expect("queue final Wikimedia render");
        let rendered = workspace
            .service
            .wait_for_job(&render.job_id, &project_id, Duration::from_secs(10 * 60))
            .expect("render final Wikimedia output");
        let master = rendered
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .and_then(|outputs| {
                outputs.iter().find(|output| {
                    output.get("kind").and_then(Value::as_str) == Some("master")
                        && output.get("is_primary").and_then(Value::as_bool) == Some(true)
                })
            })
            .expect("playable final master");
        let master_path =
            PathBuf::from(value_string(master, "artifact_path").expect("master path"));
        let master_probe = probe_media(
            &master_path,
            runtime.ffprobe.path.as_deref().expect("ffprobe path"),
        )
        .expect("probe final master");
        assert!(master_probe.primary_video_stream.is_some());
        let package = workspace
            .service
            .export_publish_package(PublishPackageRequest {
                project_id,
                expected_revision: None,
                expected_version_id: None,
                destination_dir: None,
                actor: "production-link-smoke".to_string(),
            })
            .expect("build managed Wikimedia publish package");
        let archive = PathBuf::from(
            package
                .get("archive_path")
                .and_then(Value::as_str)
                .expect("package archive"),
        );
        validate_publish_zip_with_cancel(
            &archive,
            &PathBuf::from(
                package
                    .get("package_path")
                    .and_then(Value::as_str)
                    .expect("package directory"),
            ),
            None,
        )
        .expect("validate complete publish ZIP");
    }

    #[test]
    fn caption_cache_key_changes_with_the_effective_canvas() {
        let mut manifest = empty_manifest("project-caption-cache", "Caption cache");
        let request = TimelineRenderRequest {
            project_id: manifest.project_id.clone(),
            expected_revision: 1,
            expected_version_id: "version-caption-cache".to_string(),
            profile: TimelineRenderProfile::Preview,
            caption_theme: CaptionTheme::Calm,
            portrait_layout: PortraitSourceLayout::CenterCrop,
            actor: "service-test".to_string(),
            variation: 0,
            include_title_cards: true,
            include_speaker_cards: true,
            burn_captions: true,
        };
        let options = AssemblyOptions {
            profile: RenderProfile::Preview,
            portrait_layout: PortraitLayout::CenterCrop,
            caption_theme: CaptionTheme::Calm,
            include_title_cards: true,
            include_speaker_cards: true,
            burn_captions: true,
        };
        let portrait_key =
            timeline_caption_cache_key(&manifest, &request).expect("portrait caption key");
        let portrait_document =
            build_ass_document(&manifest, &options).expect("portrait caption document");

        manifest.layout.mode = CanvasMode::Custom;
        manifest.layout.canvas.width = 864;
        manifest.layout.canvas.height = 1_080;
        manifest.validate_strict().expect("valid custom canvas");
        let custom_key =
            timeline_caption_cache_key(&manifest, &request).expect("custom caption key");
        let custom_document =
            build_ass_document(&manifest, &options).expect("custom caption document");

        assert_ne!(portrait_document, custom_document);
        assert_ne!(
            portrait_key, custom_key,
            "layout-dependent ASS documents must never share a cache key"
        );

        let before_geometry = manifest.clone();
        manifest.layout.elements.push(super::super::LayoutElement {
            id: "caption-layout-cache".to_string(),
            role: LayoutRole::Captions,
            scene_id: None,
            bounds: super::super::NormalizedRect {
                x_bp: 100,
                y_bp: 200,
                width_bp: 3_600,
                height_bp: 1_200,
            },
            z_index: 100,
            style_id: None,
        });
        manifest.validate_strict().expect("valid caption geometry");
        let geometry_key =
            timeline_caption_cache_key(&manifest, &request).expect("geometry caption key");
        assert_ne!(custom_key, geometry_key);
        let geometry_paths =
            manifest_changed_paths(&before_geometry, &manifest).expect("caption geometry diff");
        assert_eq!(
            geometry_paths,
            BTreeSet::from(["/layout/elements/captions".to_string()])
        );
        assert_eq!(
            invalidated_stages_for_manifest_changes(&geometry_paths),
            BTreeSet::from([
                RevisionStage::Captions,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        );

        manifest.captions.push(super::super::CaptionCue {
            id: "caption-cache-cue".to_string(),
            range: TimeRange::new(0, 1_000_000).expect("caption range"),
            text: "Cache the exact curated caption plan.".to_string(),
            style_id: "caption-podcast".to_string(),
            speaker_id: None,
            transcript_segment_id: None,
            scene_id: None,
        });
        manifest.validate_strict().expect("valid caption cache cue");
        let podcast_key =
            timeline_caption_cache_key(&manifest, &request).expect("podcast caption key");
        manifest.captions[0].style_id = "caption-karaoke".to_string();
        let karaoke_key =
            timeline_caption_cache_key(&manifest, &request).expect("karaoke caption key");
        assert_ne!(geometry_key, podcast_key);
        assert_ne!(
            podcast_key, karaoke_key,
            "per-cue preset changes must invalidate the caption document cache"
        );
    }

    #[test]
    fn cancellation_updates_the_durable_job_and_stops_the_worker() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Cancellation contract");
        let job_id = workspace
            .service
            .store
            .create_job(
                "video_test_cancellation",
                &json!({ "project_id": project_id, "priority": "normal" }),
            )
            .expect("durable job");
        workspace
            .service
            .spawn_worker(
                job_id.clone(),
                project_id,
                None,
                move |service, job_id, cancel, _| {
                    service
                        .store
                        .start_job(&job_id)
                        .map_err(VideoServiceError::store)?;
                    for _ in 0..500 {
                        service.ensure_not_cancelled(&cancel)?;
                        thread::sleep(Duration::from_millis(2));
                    }
                    Ok(())
                },
            )
            .expect("start cancellable worker");
        thread::sleep(Duration::from_millis(20));
        assert!(workspace.service.cancel_job(&job_id).expect("cancel job"));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let job = workspace
                .service
                .store
                .get_job(&job_id)
                .expect("load durable job")
                .expect("job exists");
            if job.get("status").and_then(Value::as_str) == Some("cancelled") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not stop after cancellation"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn real_ffmpeg_narration_revision_preserves_scene_clock_and_is_idempotent() {
        let workspace = TestWorkspace::new();
        let runtime = workspace.service.runtime_status(true);
        if !runtime.ready_for_local_media {
            eprintln!("Skipping narration revision smoke because FFmpeg/FFprobe are unavailable");
            return;
        }
        let ffmpeg = runtime.ffmpeg.path.as_deref().expect("ffmpeg path");
        let ffprobe = runtime.ffprobe.path.as_deref().expect("ffprobe path");
        let script = "A calm replacement voice keeps this exact scene clock.";
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Narration revision");

        let original_audio = workspace.root.join("original-narration.wav");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:sample_rate=48000:duration=1.5",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&original_audio)
            .status()
            .expect("generate original narration fixture");
        assert!(status.success());
        let import_job = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id: project_id.clone(),
                    source_path: original_audio,
                    actor: "service-test".to_string(),
                    title: Some("Original narration".to_string()),
                },
                None,
            )
            .expect("queue narration source");
        let _imported = workspace
            .service
            .wait_for_job(&import_job.job_id, &project_id, Duration::from_secs(120))
            .expect("import original narration");
        let scene_id = "scene-narration-one".to_string();
        let prepared = workspace
            .service
            .commit_manifest_mutation(
                &project_id,
                "service-test",
                "Bind the original narration clip to its reviewed scene",
                Some("ready"),
                vec!["/reviewed_scenes".to_string(), "/tracks".to_string()],
                BTreeSet::from([
                    RevisionStage::Speech,
                    RevisionStage::SceneRender,
                    RevisionStage::Preview,
                    RevisionStage::FinalRender,
                    RevisionStage::PublishPackage,
                ]),
                {
                    let scene_id = scene_id.clone();
                    move |manifest| {
                        let duration = manifest.timeline_duration_us;
                        manifest.reviewed_scenes = vec![ReviewedScene {
                            id: scene_id.clone(),
                            candidate_id: None,
                            source_asset_id: None,
                            source_range: None,
                            timeline_start_us: Microseconds::ZERO,
                            timeline_duration_us: duration,
                            title: "Narration scene".to_string(),
                            script: script.to_string(),
                            review_state: ReviewState::Approved,
                            revision: 1,
                        }];
                        let clip = manifest
                            .tracks
                            .iter_mut()
                            .find(|track| matches!(track.kind, TrackKind::Audio))
                            .and_then(|track| track.clips.first_mut())
                            .ok_or_else(|| {
                                VideoServiceError::new(
                                    "video.test_fixture_invalid",
                                    "Imported audio did not create a timeline clip",
                                )
                            })?;
                        clip.scene_id = Some(scene_id);
                        Ok(())
                    }
                },
            )
            .expect("prepare reviewed narration scene");
        let before: VideoProjectManifest = serde_json::from_value(
            prepared
                .get("manifest")
                .cloned()
                .expect("prepared manifest"),
        )
        .expect("typed prepared manifest");
        let original_clip = before
            .tracks
            .iter()
            .find(|track| matches!(track.kind, TrackKind::Audio))
            .and_then(|track| track.clips.first())
            .expect("original narration clip")
            .clone();

        let history_audio = workspace
            .root
            .join("artifacts")
            .join("replacement-voice.wav");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=24000:duration=0.8",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&history_audio)
            .status()
            .expect("generate replacement History fixture");
        assert!(status.success());
        let synthesis_request = json!({
            "model_id": "test/voice-model",
            "text": script,
            "speaker": "speaker-new",
            "voice_name": "New voice",
            "language": "en",
            "generation_kind": "speech",
            "output_format": "wav",
        });
        let synthesis_job = workspace
            .service
            .store
            .create_job("synthesis", &synthesis_request)
            .expect("create synthesis fixture job");
        workspace
            .service
            .store
            .start_job(&synthesis_job)
            .expect("start synthesis fixture job");
        let history_id = format!("history-{}", new_id());
        workspace
            .service
            .store
            .complete_synthesis(
                &synthesis_job,
                &synthesis_request,
                &json!({
                    "id": history_id,
                    "model_id": "test/voice-model",
                    "engine": "service-test",
                    "audio_path": history_audio,
                    "sample_rate": 24_000,
                    "duration_seconds": 0.8,
                    "inference_seconds": 0.1,
                    "rtf": 0.125,
                    "vram_peak_mb": 0,
                    "waveform": [0.2, 0.8],
                }),
            )
            .expect("register replacement in History");
        let expectation = project_expectation(&prepared).expect("prepared expectation");
        let parent_job_id = workspace
            .service
            .store
            .create_job(
                "video_regenerate_narration",
                &json!({ "project_id": project_id, "scene_id": scene_id }),
            )
            .expect("create narration parent job");
        workspace
            .service
            .store
            .start_job(&parent_job_id)
            .expect("start narration parent job");
        let request = ReplaceNarrationRequest {
            project_id: project_id.clone(),
            expected_revision: expectation.revision,
            expected_version_id: expectation.version_id,
            actor: "service-test".to_string(),
            parent_job_id: Some(parent_job_id),
            replacements: vec![NarrationReplacement {
                binding_id: None,
                fidelity: TakeFidelity::Final,
                scene_id: Some(scene_id.clone()),
                turn_id: None,
                clip_id: None,
                history_id: history_id.clone(),
                voice_id: "voice-new".to_string(),
                model_id: "test/voice-model".to_string(),
                speaker: "speaker-new".to_string(),
                language: "en".to_string(),
                performance: None,
            }],
        };
        let replacement = workspace
            .service
            .queue_narration_replacement(request.clone(), None)
            .expect("queue narration replacement");
        let replaced = workspace
            .service
            .wait_for_job(&replacement.job_id, &project_id, Duration::from_secs(120))
            .expect("narration replacement completes");
        let manifest: VideoProjectManifest =
            serde_json::from_value(replaced.project.get("manifest").cloned().expect("manifest"))
                .expect("typed replaced manifest");
        assert_eq!(manifest.timeline_duration_us, before.timeline_duration_us);
        let binding = manifest
            .narration_bindings
            .iter()
            .find(|binding| binding.scene_id.as_deref() == Some(scene_id.as_str()))
            .expect("active narration binding");
        assert_eq!(binding.history_id, history_id);
        assert_eq!(binding.voice_id, "voice-new");
        let replaced_clip = manifest
            .tracks
            .iter()
            .find(|track| matches!(track.kind, TrackKind::Audio))
            .and_then(|track| track.clips.first())
            .expect("replaced narration clip");
        assert_eq!(
            replaced_clip.media.render_artifact_id.as_deref(),
            Some(binding.render_artifact_id.as_str())
        );
        assert_eq!(
            replaced_clip.timeline_start_us,
            original_clip.timeline_start_us
        );
        assert_eq!(
            replaced_clip.timeline_duration_us,
            original_clip.timeline_duration_us
        );
        assert_eq!(replaced_clip.playback_rate, RationalRate::ONE);
        let artifact = manifest
            .render_artifacts
            .iter()
            .find(|artifact| artifact.id == binding.render_artifact_id)
            .expect("conformed narration artifact");
        assert_eq!(
            artifact.duration_us,
            Some(original_clip.timeline_duration_us)
        );
        let conformed_path = workspace
            .service
            .resolve_managed_path(&artifact.managed_path)
            .expect("managed conformed narration");
        let conformed_probe = probe_media(&conformed_path, ffprobe).expect("probe narration");
        assert!(
            (conformed_probe.duration_us - original_clip.timeline_duration_us.0).abs() <= 50_000,
            "replacement must preserve the source-clock scene duration"
        );
        let invalidated = &manifest
            .revision_history
            .last()
            .expect("narration revision")
            .invalidated_stages;
        assert_eq!(
            invalidated,
            &BTreeSet::from([
                RevisionStage::Speech,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ])
        );

        let adopted = workspace
            .service
            .queue_narration_replacement(request, None)
            .expect("adopt durable narration child");
        assert_eq!(adopted.job_id, replacement.job_id);
        let adopted = workspace
            .service
            .wait_for_job(&adopted.job_id, &project_id, Duration::from_secs(5))
            .expect("adopted child remains complete");
        assert_eq!(
            adopted.project.get("revision").and_then(Value::as_i64),
            replaced.project.get("revision").and_then(Value::as_i64),
            "idempotent adoption must not create another manifest revision"
        );

        let before_cancel = workspace
            .service
            .get_project(&project_id)
            .expect("project before parent cancellation");
        let cancel_expectation =
            project_expectation(&before_cancel).expect("cancellation expectation");
        let cancel_parent_job_id = workspace
            .service
            .store
            .create_job(
                "video_regenerate_narration",
                &json!({ "project_id": project_id, "scene_id": scene_id }),
            )
            .expect("create cancellation parent job");
        workspace
            .service
            .store
            .start_job(&cancel_parent_job_id)
            .expect("start cancellation parent job");
        let cancel_request = ReplaceNarrationRequest {
            project_id: project_id.clone(),
            expected_revision: cancel_expectation.revision,
            expected_version_id: cancel_expectation.version_id,
            actor: "service-test".to_string(),
            parent_job_id: Some(cancel_parent_job_id.clone()),
            replacements: vec![NarrationReplacement {
                binding_id: None,
                fidelity: TakeFidelity::Final,
                scene_id: Some(scene_id.clone()),
                turn_id: None,
                clip_id: None,
                history_id: history_id.clone(),
                voice_id: "voice-cancelled-before-commit".to_string(),
                model_id: "test/voice-model".to_string(),
                speaker: "speaker-new".to_string(),
                language: "en".to_string(),
                performance: None,
            }],
        };
        let cancellation_observed = Arc::new(AtomicBool::new(false));
        let callback_service = Arc::clone(&workspace.service);
        let callback_parent_id = cancel_parent_job_id.clone();
        let callback_observed = Arc::clone(&cancellation_observed);
        let cancel_callback: ProgressCallback = Arc::new(move |progress| {
            if progress.phase == "narration_committing"
                && !callback_observed.swap(true, Ordering::AcqRel)
            {
                let _ = callback_service.cancel_job(&callback_parent_id);
            }
        });
        let cancelled = workspace
            .service
            .queue_narration_replacement(cancel_request, Some(cancel_callback))
            .expect("queue cancellation narration replacement");
        let cancelled_error = workspace
            .service
            .wait_for_job(&cancelled.job_id, &project_id, Duration::from_secs(120))
            .expect_err("parent cancellation must stop the child before commit");
        assert_eq!(cancelled_error.code, "video.cancelled");
        assert!(
            cancellation_observed.load(Ordering::Acquire),
            "the test must cancel after the service precheck and at the atomic manifest boundary"
        );
        let after_cancel = workspace
            .service
            .get_project(&project_id)
            .expect("project after parent cancellation");
        assert_eq!(
            project_expectation(&after_cancel).expect("unchanged expectation"),
            project_expectation(&before_cancel).expect("baseline expectation"),
            "parent cancellation must leave the project revision and version unchanged"
        );
        let after_cancel_manifest: VideoProjectManifest =
            serde_json::from_value(after_cancel.get("manifest").cloned().expect("manifest"))
                .expect("typed manifest after parent cancellation");
        assert!(
            after_cancel_manifest
                .narration_bindings
                .iter()
                .all(|binding| { binding.voice_id != "voice-cancelled-before-commit" }),
            "a cancelled parent must never publish the prepared narration binding"
        );
    }

    #[test]
    fn generated_history_audio_origin_survives_cancelled_restart_and_resume() {
        let workspace = TestWorkspace::new();
        let runtime = workspace.service.runtime_status(true);
        if !runtime.ready_for_local_media {
            eprintln!("Skipping generated-origin smoke because FFmpeg/FFprobe are unavailable");
            return;
        }
        let ffmpeg = runtime.ffmpeg.path.as_deref().expect("ffmpeg path");
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Prompt-generated narration import");
        let generated_audio = workspace
            .root
            .join("artifacts")
            .join("prompt-generated-history.wav");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=510:sample_rate=24000:duration=1.2",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&generated_audio)
            .status()
            .expect("generate prompt History fixture");
        assert!(status.success());
        let original_generated_audio = fs::read(&generated_audio).expect("read History fixture");
        let synthesis_request = json!({
            "model_id": "test/prompt-voice-model",
            "text": "A prompt-generated narration artifact.",
            "speaker": "prompt-speaker",
            "voice_name": "Prompt voice",
            "language": "en",
            "generation_kind": "speech",
            "output_format": "wav",
        });
        let synthesis_job = workspace
            .service
            .store
            .create_job("synthesis", &synthesis_request)
            .expect("create synthesis origin job");
        workspace
            .service
            .store
            .start_job(&synthesis_job)
            .expect("start synthesis origin job");
        let history_id = format!("history-{}", new_id());
        workspace
            .service
            .store
            .complete_synthesis(
                &synthesis_job,
                &synthesis_request,
                &json!({
                    "id": history_id,
                    "model_id": "test/prompt-voice-model",
                    "engine": "service-test-prompt-engine",
                    "audio_path": generated_audio,
                    "sample_rate": 24_000,
                    "duration_seconds": 1.2,
                    "inference_seconds": 0.1,
                    "rtf": 0.0833,
                    "vram_peak_mb": 0,
                    "waveform": [0.1, 0.7, 0.2],
                }),
            )
            .expect("register prompt-generated History artifact");

        // Pause after source/derived asset upserts but before the parent-
        // guarded manifest transaction. Cancelling here reproduces the exact
        // crash window where random identities used to leak duplicate rows and
        // managed files on same-child resume.
        let prompt_parent_job = workspace
            .service
            .store
            .create_job(
                "video_create_from_prompt",
                &json!({
                    "project_id": project_id,
                    "purpose": "prompt_to_video",
                    "prompt": "Create a short narrated video",
                }),
            )
            .expect("create prompt-to-video parent");
        workspace
            .service
            .store
            .start_job(&prompt_parent_job)
            .expect("start prompt-to-video parent");
        let local_request = LocalImportRequest {
            project_id: project_id.clone(),
            source_path: generated_audio.clone(),
            actor: "service-test".to_string(),
            title: Some("Prompt narration".to_string()),
        };
        let asset_upsert_barrier = Arc::new(std::sync::Barrier::new(2));
        *workspace
            .service
            .local_import_test_barrier
            .lock()
            .expect("local import crash barrier") = Some(Arc::clone(&asset_upsert_barrier));
        let queued = workspace
            .service
            .queue_local_import_idempotent(local_request.clone(), &prompt_parent_job, None)
            .expect("queue prompt-generated History import");
        asset_upsert_barrier.wait();
        let assets_before_cancel = workspace
            .service
            .store
            .list_video_assets(&project_id)
            .expect("assets staged before manifest commit");
        assert_eq!(
            assets_before_cancel.len(),
            2,
            "audio import stages one speech source and one waveform row"
        );
        assert!(assets_before_cancel
            .iter()
            .all(|asset| { asset.get("status").and_then(Value::as_str) == Some("pending") }));
        let staged_asset_ids = assets_before_cancel
            .iter()
            .map(|asset| value_string(asset, "id").expect("staged asset id"))
            .collect::<BTreeSet<_>>();
        let staged_asset_paths = assets_before_cancel
            .iter()
            .filter_map(|asset| asset.get("local_path").and_then(Value::as_str))
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>();
        assert!(staged_asset_paths.iter().all(|path| path.is_file()));
        assert!(workspace
            .service
            .cancel_job(&queued.job_id)
            .expect("cancel after asset upsert"));
        asset_upsert_barrier.wait();
        let cancelled = workspace
            .service
            .wait_for_job(&queued.job_id, &project_id, Duration::from_secs(30))
            .expect_err("first import attempt is cancelled before manifest commit");
        assert_eq!(cancelled.code, "video.cancelled");
        let cancelled_project = workspace
            .service
            .get_project(&project_id)
            .expect("project remains available after crash-window cancellation");
        let cancelled_manifest: VideoProjectManifest = serde_json::from_value(
            cancelled_project
                .get("manifest")
                .cloned()
                .expect("cancelled import manifest"),
        )
        .expect("typed cancelled import manifest");
        assert!(
            cancelled_manifest.source_assets.is_empty(),
            "cancellation after upsert must still stop the guarded manifest commit"
        );

        let managed_sha_before_changed_retry = staged_asset_paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    sha256_file(path).expect("staged managed checksum before changed retry"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        OpenOptions::new()
            .append(true)
            .open(&generated_audio)
            .and_then(|mut file| file.write_all(b"changed-before-durable-import-retry"))
            .expect("change local History bytes before retry");
        let changed_retry = workspace
            .service
            .resume_job(&queued.job_id, None)
            .expect("dispatch exact changed-content retry");
        let changed_error = workspace
            .service
            .wait_for_job(&changed_retry.job_id, &project_id, Duration::from_secs(30))
            .expect_err("changed local bytes must reject durable import retry");
        assert_eq!(changed_error.code, "video.job_failed");
        let assets_after_changed_retry = workspace
            .service
            .store
            .list_video_assets(&project_id)
            .expect("asset rows after changed retry rejection");
        assert_eq!(
            assets_after_changed_retry, assets_before_cancel,
            "changed retry must not rewrite a pending source or derived asset row"
        );
        for (path, expected_sha256) in &managed_sha_before_changed_retry {
            assert_eq!(
                sha256_file(path).expect("managed checksum after changed retry"),
                *expected_sha256,
                "changed retry must not overwrite any managed source/cache inode"
            );
        }
        let changed_project = workspace
            .service
            .get_project(&project_id)
            .expect("project after changed retry rejection");
        assert_eq!(
            project_expectation(&changed_project).expect("changed retry expectation"),
            project_expectation(&cancelled_project).expect("cancelled baseline expectation")
        );
        fs::write(&generated_audio, &original_generated_audio)
            .expect("restore exact registered History bytes");

        let resumed = workspace
            .service
            .queue_local_import_idempotent(local_request.clone(), &prompt_parent_job, None)
            .expect("retry adopts and rearms the durable generated import");
        assert_eq!(
            resumed.job_id, queued.job_id,
            "the parent-bound retry must reuse the exact child job"
        );
        let imported = workspace
            .service
            .wait_for_job(&resumed.job_id, &project_id, Duration::from_secs(120))
            .expect("resumed generated import completes");
        let adopted = workspace
            .service
            .queue_local_import_idempotent(local_request, &prompt_parent_job, None)
            .expect("retry after child completion adopts without another import");
        assert_eq!(adopted.job_id, queued.job_id);
        assert_eq!(
            workspace
                .service
                .store
                .video_child_job(&prompt_parent_job, "video_import_local")
                .expect("resolve parent-bound import child"),
            Some((queued.job_id.clone(), "completed".to_string()))
        );
        let manifest: VideoProjectManifest =
            serde_json::from_value(imported.project.get("manifest").cloned().expect("manifest"))
                .expect("typed generated import manifest");
        let source = manifest
            .source_assets
            .iter()
            .find(|source| matches!(source.kind, SourceAssetKind::SoundArSpeech))
            .expect("soundAr speech source");
        assert_eq!(
            source.id,
            stable_import_asset_id(&project_id, &queued.job_id, "source", "local-primary")
        );
        assert_eq!(source.provenance.kind, ProvenanceKind::GeneratedLocally);
        assert_eq!(
            source
                .provenance
                .metadata
                .get("history_id")
                .and_then(Value::as_str),
            Some(history_id.as_str())
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("generation_job_id")
                .and_then(Value::as_str),
            Some(synthesis_job.as_str())
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("model_id")
                .and_then(Value::as_str),
            Some("test/prompt-voice-model")
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("voice")
                .and_then(Value::as_str),
            Some("Prompt voice")
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("video_parent_job_id")
                .and_then(Value::as_str),
            Some(prompt_parent_job.as_str())
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("video_project_id")
                .and_then(Value::as_str),
            Some(project_id.as_str())
        );
        assert_eq!(
            source
                .provenance
                .metadata
                .get("purpose")
                .and_then(Value::as_str),
            Some("prompt_to_video")
        );
        let assets = workspace
            .service
            .store
            .list_video_assets(&project_id)
            .expect("list generated video assets");
        assert_eq!(
            assets
                .iter()
                .map(|asset| value_string(asset, "id").expect("resumed asset id"))
                .collect::<BTreeSet<_>>(),
            staged_asset_ids,
            "same-child resume must adopt every deterministic asset row"
        );
        assert_eq!(
            assets
                .iter()
                .filter_map(|asset| asset.get("local_path").and_then(Value::as_str))
                .map(PathBuf::from)
                .collect::<BTreeSet<_>>(),
            staged_asset_paths,
            "same-child resume must reuse managed files rather than leaking copies"
        );
        assert!(assets
            .iter()
            .all(|asset| asset.get("status").and_then(Value::as_str) == Some("ready")));
        assert!(assets.iter().any(|asset| {
            asset.get("id").and_then(Value::as_str) == Some(source.id.as_str())
                && asset.get("kind").and_then(Value::as_str) == Some("speech")
                && asset.get("source_kind").and_then(Value::as_str) == Some("generated")
        }));
        assert_eq!(
            manifest
                .source_assets
                .iter()
                .filter(|candidate| {
                    candidate
                        .provenance
                        .metadata
                        .get("history_id")
                        .and_then(Value::as_str)
                        == Some(history_id.as_str())
                })
                .count(),
            1,
            "resume must import one managed speech source, never duplicate History"
        );

        // Re-enter the exact publication boundary after the source is fully
        // bound in both Store and manifest. A changed retry with a different
        // inferred extension must not touch the canonical row, revision, or
        // inode; a missing canonical inode must fail closed as corruption
        // rather than silently recreating it from potentially changed input.
        let bound_project_before = workspace
            .service
            .get_project(&project_id)
            .expect("bound import project snapshot");
        let bound_expectation =
            project_expectation(&bound_project_before).expect("bound import expectation");
        let bound_source_row = assets
            .iter()
            .find(|asset| asset.get("id").and_then(Value::as_str) == Some(source.id.as_str()))
            .cloned()
            .expect("bound speech asset row");
        let bound_source_path = workspace
            .service
            .resolve_managed_path(&source.managed_path)
            .expect("bound managed speech path");
        let bound_source_inode = fs::metadata(&bound_source_path)
            .expect("bound source metadata")
            .ino();
        let bound_source_sha256 = sha256_file(&bound_source_path).expect("bound source checksum");
        let bound_source_dir = bound_source_path.parent().expect("bound source parent");
        let alternate_target = bound_source_dir.join(format!("source-{}.mp3", source.id));
        let bound_changed_staging =
            bound_source_dir.join(format!(".{}.bound-changed.partial", source.id));
        fs::write(&bound_changed_staging, b"changed-bound-import-bytes")
            .expect("stage changed bound retry");
        let bound_changed_sha256 =
            sha256_file(&bound_changed_staging).expect("changed bound staging checksum");
        let bound_error = workspace
            .service
            .publish_import_staging_once(
                &project_id,
                &source.id,
                &bound_changed_staging,
                &alternate_target,
                &bound_changed_sha256,
                &AtomicBool::new(false),
                |_| Ok(()),
            )
            .expect_err("changed bytes cannot replace a manifest-bound source");
        assert_eq!(bound_error.code, "video.import_identity_conflict");
        assert_eq!(
            fs::metadata(&bound_source_path)
                .expect("bound source after rejection")
                .ino(),
            bound_source_inode
        );
        assert_eq!(
            sha256_file(&bound_source_path).expect("bound checksum after rejection"),
            bound_source_sha256
        );
        assert!(!alternate_target.exists());
        assert_eq!(
            workspace
                .service
                .store
                .list_video_assets(&project_id)
                .expect("bound rows after rejection")
                .into_iter()
                .find(|asset| {
                    asset.get("id").and_then(Value::as_str) == Some(source.id.as_str())
                })
                .expect("bound source row after rejection"),
            bound_source_row
        );
        let bound_project_after = workspace
            .service
            .get_project(&project_id)
            .expect("bound project after rejection");
        assert_eq!(
            project_expectation(&bound_project_after).expect("bound expectation after rejection"),
            bound_expectation
        );
        assert_eq!(
            bound_project_after.get("manifest"),
            bound_project_before.get("manifest")
        );

        fs::remove_file(&bound_source_path).expect("simulate missing bound managed source");
        let missing_retry_staging =
            bound_source_dir.join(format!(".{}.missing-retry.partial", source.id));
        fs::write(&missing_retry_staging, &original_generated_audio)
            .expect("stage retry for missing bound source");
        let missing_retry_sha256 =
            sha256_file(&missing_retry_staging).expect("missing retry checksum");
        let missing_error = workspace
            .service
            .publish_import_staging_once(
                &project_id,
                &source.id,
                &missing_retry_staging,
                &alternate_target,
                &missing_retry_sha256,
                &AtomicBool::new(false),
                |_| Ok(()),
            )
            .expect_err("a missing manifest-bound source must fail closed");
        assert!(matches!(
            missing_error.code.as_str(),
            "video.artifact_not_found" | "video.integrity_failed"
        ));
        assert!(!missing_retry_staging.exists());
        assert!(!alternate_target.exists());
        assert_eq!(
            workspace
                .service
                .store
                .list_video_assets(&project_id)
                .expect("bound rows after missing-file rejection")
                .into_iter()
                .find(|asset| {
                    asset.get("id").and_then(Value::as_str) == Some(source.id.as_str())
                })
                .expect("bound source row remains after missing-file rejection"),
            bound_source_row
        );
        let missing_project_after = workspace
            .service
            .get_project(&project_id)
            .expect("project after missing bound source rejection");
        assert_eq!(
            project_expectation(&missing_project_after).expect("missing source expectation"),
            bound_expectation
        );
        assert_eq!(
            missing_project_after.get("manifest"),
            bound_project_before.get("manifest")
        );

        let tamper_project_id = format!("project-{}", new_id());
        workspace.create_project(&tamper_project_id, "Tampered History rejection");
        let tamper_project_before = workspace
            .service
            .get_project(&tamper_project_id)
            .expect("tamper project baseline");
        let tampered_request = DurableLocalImportRequest {
            project_id: tamper_project_id.clone(),
            source_path: generated_audio.clone(),
            actor: "service-test".to_string(),
            title: Some("Tampered prompt narration".to_string()),
            origin: DurableLocalImportOrigin::SoundArHistory {
                history_id: history_id.clone(),
                generation_job_id: synthesis_job.clone(),
                generation_kind: "speech".to_string(),
                model_id: "test/prompt-voice-model".to_string(),
                voice: "Prompt voice".to_string(),
                engine: "service-test-prompt-engine".to_string(),
            },
            parent_job_id: None,
            priority: default_normal_priority(),
        };
        let tampered_job_id = workspace
            .service
            .store
            .create_job(
                "video_import_local",
                &serde_json::to_value(&tampered_request).expect("durable tamper request"),
            )
            .expect("create durable tamper import");
        assert!(workspace
            .service
            .store
            .cancel_job(&tampered_job_id)
            .expect("cancel durable tamper import"));
        let (_, resumed_tamper_request) = workspace
            .service
            .store
            .resume_video_job(&tampered_job_id, &["video_import_local"])
            .expect("rearm durable tamper import");
        OpenOptions::new()
            .append(true)
            .open(&generated_audio)
            .and_then(|mut file| file.write_all(b"tampered-after-queue"))
            .expect("tamper registered History after durable queueing");
        workspace
            .service
            .dispatch_resumed_job(
                tampered_job_id.clone(),
                "video_import_local",
                resumed_tamper_request,
                None,
            )
            .expect("dispatch tampered durable import");
        let tamper_error = workspace
            .service
            .wait_for_job(
                &tampered_job_id,
                &tamper_project_id,
                Duration::from_secs(30),
            )
            .expect_err("tampered History must fail exact path/integrity revalidation");
        assert_eq!(tamper_error.code, "video.job_failed");
        assert!(
            tamper_error
                .message
                .contains("registered soundAr audio changed"),
            "the durable resume must report the History integrity failure: {}",
            tamper_error.message
        );
        let tamper_project_after = workspace
            .service
            .get_project(&tamper_project_id)
            .expect("tamper project after rejection");
        assert_eq!(
            project_expectation(&tamper_project_after).expect("tamper current expectation"),
            project_expectation(&tamper_project_before).expect("tamper baseline expectation"),
            "a changed History artifact must not advance the video project"
        );
        let tamper_manifest: VideoProjectManifest = serde_json::from_value(
            tamper_project_after
                .get("manifest")
                .cloned()
                .expect("tamper manifest"),
        )
        .expect("typed tamper manifest");
        assert!(tamper_manifest.source_assets.is_empty());
    }

    #[test]
    fn real_ffmpeg_release_export_produces_registered_playable_deliverables() {
        let workspace = TestWorkspace::new();
        let runtime = workspace.service.runtime_status(true);
        if !runtime.ready_for_local_media {
            eprintln!("Skipping release export test because FFmpeg/FFprobe are unavailable");
            return;
        }
        let ffmpeg = runtime
            .ffmpeg
            .path
            .as_deref()
            .expect("ffmpeg path")
            .to_path_buf();
        let ffprobe = runtime
            .ffprobe
            .path
            .as_deref()
            .expect("ffprobe path")
            .to_path_buf();

        let project_id = format!("project-{}", new_id());
        let created = workspace.create_project(&project_id, "Release export");
        let mut manifest: VideoProjectManifest =
            serde_json::from_value(created.get("manifest").cloned().expect("manifest")).unwrap();

        // A finished master is the one hard prerequisite; every deliverable derives from it.
        let master_dir = workspace
            .service
            .project_dir(&project_id)
            .expect("project dir")
            .join("renders");
        fs::create_dir_all(&master_dir).expect("create render dir");
        let master_path = master_dir.join("master.mp4");
        let status = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=4",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=4",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&master_path)
            .status()
            .expect("render fixture master");
        assert!(status.success(), "the fixture master could not be rendered");

        manifest.timeline_duration_us = Microseconds(4_000_000);
        manifest.reviewed_scenes.push(ReviewedScene {
            id: "scene-one".into(),
            candidate_id: None,
            source_asset_id: None,
            source_range: None,
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(4_000_000),
            title: "Act 1; the letter".into(),
            script: "The harmattan came early.".into(),
            review_state: ReviewState::Approved,
            revision: 1,
        });
        manifest.render_artifacts.push(RenderArtifact {
            id: "master".into(),
            role: RenderArtifactRole::FinalMaster,
            scene_id: None,
            managed_path: workspace
                .service
                .relative_managed_path(&master_path)
                .expect("relative master path"),
            sha256: sha256_file(&master_path).expect("hash master"),
            cache_key: "c".repeat(64),
            mime_type: "video/mp4".into(),
            duration_us: Some(Microseconds(4_000_000)),
            width: Some(320),
            height: Some(180),
            publication_state: PublicationState::Published,
            created_at: utc_now(),
        });
        manifest.revision += 1;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: manifest.revision,
            parent_id: manifest
                .revision_history
                .last()
                .map(|record| record.id.clone()),
            actor: "service-test".into(),
            reason: "Attach a finished master".into(),
            changed_paths: vec![
                "/render_artifacts".into(),
                "/reviewed_scenes".into(),
                "/timeline_duration_us".into(),
            ],
            invalidated_stages: BTreeSet::from([
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict().expect("valid master manifest");
        workspace
            .service
            .revise_manifest(ReviseVideoManifestRequest {
                project_id: project_id.clone(),
                expected_revision: created.get("revision").and_then(Value::as_i64).unwrap(),
                manifest,
                actor: "service-test".into(),
                reason: "Attach a finished master".into(),
                changed_paths: vec![
                    "/render_artifacts".into(),
                    "/reviewed_scenes".into(),
                    "/timeline_duration_us".into(),
                ],
                invalidated_stages: BTreeSet::from([
                    RevisionStage::Plan,
                    RevisionStage::Captions,
                    RevisionStage::Tracking,
                    RevisionStage::SceneRender,
                    RevisionStage::Preview,
                    RevisionStage::FinalRender,
                    RevisionStage::PublishPackage,
                ]),
                status: Some("ready".into()),
            })
            .expect("commit the master");

        let exported = workspace
            .service
            .export_episode_release(&project_id, "service-test", true)
            .expect("export the release");

        // The audio episode and the audiogram always derive from the master; the trailer needs a
        // narrated moment, which this fixture has none of, so it is reported rather than omitted.
        let produced = exported
            .produced
            .iter()
            .map(|member| member.kind)
            .collect::<Vec<_>>();
        assert!(produced.contains(&ReleaseMemberKind::PodcastAudio));
        assert!(produced.contains(&ReleaseMemberKind::Audiogram));
        assert!(exported
            .skipped
            .iter()
            .any(|member| member.kind == ReleaseMemberKind::Trailer
                && member.blocked_reason.is_some()));

        for member in &exported.produced {
            let path = workspace
                .service
                .resolve_absolute_managed_path(
                    &workspace.service.video_root.join(&member.managed_path),
                )
                .expect("resolve deliverable");
            // Registered means checksummed and playable, not merely written.
            assert_eq!(sha256_file(&path).expect("hash deliverable"), member.sha256);
            let probe = probe_media(&path, &ffprobe).expect("probe deliverable");
            assert!(probe.duration_us > 0);
            match member.kind {
                ReleaseMemberKind::PodcastAudio => {
                    assert!(probe.primary_video_stream.is_none());
                    assert_eq!(probe.chapters.len(), 1);
                    // A chapter title containing FFmetadata syntax survives the round trip.
                    assert_eq!(
                        probe.chapters[0].title.as_deref(),
                        Some("Act 1; the letter")
                    );
                }
                ReleaseMemberKind::Audiogram => {
                    assert!(probe.primary_video_stream.is_some());
                    assert!(probe.primary_audio_stream.is_some());
                }
                other => panic!("unexpected deliverable {other:?}"),
            }
        }

        // Every deliverable is addressable on the committed project, not just on disk.
        let reloaded: VideoProjectManifest =
            serde_json::from_value(exported.project.get("manifest").cloned().expect("manifest"))
                .unwrap();
        for member in &exported.produced {
            assert!(reloaded
                .render_artifacts
                .iter()
                .any(|artifact| artifact.id == member.artifact_id
                    && matches!(artifact.publication_state, PublicationState::Published)));
        }
    }

    #[test]
    fn a_release_cannot_be_exported_from_stand_in_takes() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Draft release");
        // No master at all: the one hard prerequisite is named rather than failing later.
        let error = workspace
            .service
            .export_episode_release(&project_id, "service-test", true)
            .expect_err("a release without a master is refused");
        assert_eq!(error.code, "video.final_master_required");
    }

    #[test]
    fn real_ffmpeg_ingest_audio_video_render_and_package_smoke() {
        let workspace = TestWorkspace::new();
        let runtime = workspace.service.runtime_status(true);
        if !runtime.ready_for_local_media {
            eprintln!("Skipping real media smoke test because FFmpeg/FFprobe are unavailable");
            return;
        }
        let ffmpeg = runtime.ffmpeg.path.as_deref().expect("ffmpeg path");

        let video_fixture = workspace.root.join("fixture-video.mp4");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=30:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-c:v",
                "mpeg4",
                "-q:v",
                "5",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(&video_fixture)
            .status()
            .expect("generate video fixture");
        assert!(status.success());
        let video_project = format!("project-{}", new_id());
        workspace.create_project(&video_project, "Imported reel");
        let import = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id: video_project.clone(),
                    source_path: video_fixture,
                    actor: "service-test".to_string(),
                    title: Some("Rights-clear generated fixture".to_string()),
                },
                None,
            )
            .expect("queue video import");
        let imported = workspace
            .service
            .wait_for_job(&import.job_id, &video_project, Duration::from_secs(120))
            .expect("video import completes");
        let imported_manifest: VideoProjectManifest =
            serde_json::from_value(imported.project.get("manifest").cloned().expect("manifest"))
                .expect("typed imported manifest");
        assert!(imported_manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.role == RenderArtifactRole::Proxy));
        assert!(imported_manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.role == RenderArtifactRole::Thumbnail));
        assert!(imported_manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.role == RenderArtifactRole::Waveform));
        for managed_path in imported_manifest
            .source_assets
            .iter()
            .map(|source| source.managed_path.as_str())
            .chain(
                imported_manifest
                    .render_artifacts
                    .iter()
                    .map(|artifact| artifact.managed_path.as_str()),
            )
        {
            let path = workspace
                .service
                .resolve_managed_path(managed_path)
                .expect("resolve private ingest artifact");
            assert_eq!(
                fs::metadata(path).expect("ingest artifact mode").mode() & 0o7777,
                PRIVATE_FILE_MODE,
                "managed source/proxy/thumbnail/waveform files stay owner-only"
            );
        }
        let first_proxy = imported_manifest
            .render_artifacts
            .iter()
            .find(|artifact| artifact.role == RenderArtifactRole::Proxy)
            .expect("first proxy")
            .clone();
        let cache_project = format!("project-{}", new_id());
        workspace.create_project(&cache_project, "Cached imported reel");
        let cached_import = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id: cache_project.clone(),
                    source_path: workspace.root.join("fixture-video.mp4"),
                    actor: "service-test".to_string(),
                    title: Some("Same rights-clear fixture".to_string()),
                },
                None,
            )
            .expect("queue cached import");
        let cached = workspace
            .service
            .wait_for_job(
                &cached_import.job_id,
                &cache_project,
                Duration::from_secs(120),
            )
            .expect("cached import completes");
        let cached_manifest: VideoProjectManifest =
            serde_json::from_value(cached.project.get("manifest").cloned().expect("manifest"))
                .expect("typed cached manifest");
        let cached_proxy = cached_manifest
            .render_artifacts
            .iter()
            .find(|artifact| artifact.role == RenderArtifactRole::Proxy)
            .expect("cached proxy");
        assert_eq!(cached_proxy.cache_key, first_proxy.cache_key);
        assert_eq!(cached_proxy.managed_path, first_proxy.managed_path);

        let source = imported_manifest
            .source_assets
            .first()
            .expect("imported source")
            .clone();
        let duration = source.probe.duration_us.0;
        let first_duration = duration * 2 / 5;
        let gap_duration = duration / 5;
        let second_timeline_start = first_duration + gap_duration;
        let second_duration = duration - second_timeline_start;
        assert!(first_duration > 0 && gap_duration > 0 && second_duration > 0);
        let scene_one = "scene-one".to_string();
        let scene_two = "scene-two".to_string();
        let source_id = source.id.clone();
        let reviewed = workspace
            .service
            .commit_manifest_mutation(
                &video_project,
                "service-test",
                "Reviewed two source-clock scenes with an editorial gap",
                Some("ready"),
                vec![
                    "/reviewed_scenes".to_string(),
                    "/tracks".to_string(),
                    "/gaps".to_string(),
                    "/captions".to_string(),
                ],
                BTreeSet::from([
                    RevisionStage::Captions,
                    RevisionStage::SceneRender,
                    RevisionStage::Preview,
                    RevisionStage::FinalRender,
                    RevisionStage::PublishPackage,
                ]),
                move |manifest| {
                    manifest.timeline_duration_us = Microseconds(duration);
                    manifest.reviewed_scenes = vec![
                        ReviewedScene {
                            id: scene_one.clone(),
                            candidate_id: None,
                            source_asset_id: Some(source_id.clone()),
                            source_range: Some(TimeRange::new(0, first_duration)?),
                            timeline_start_us: Microseconds::ZERO,
                            timeline_duration_us: Microseconds(first_duration),
                            title: "First scene".to_string(),
                            script: "A first source-clock scene".to_string(),
                            review_state: ReviewState::Approved,
                            revision: 1,
                        },
                        ReviewedScene {
                            id: scene_two.clone(),
                            candidate_id: None,
                            source_asset_id: Some(source_id.clone()),
                            source_range: Some(TimeRange::new(
                                first_duration,
                                first_duration + second_duration,
                            )?),
                            timeline_start_us: Microseconds(second_timeline_start),
                            timeline_duration_us: Microseconds(second_duration),
                            title: "Second scene".to_string(),
                            script: "A second scene after a deliberate gap".to_string(),
                            review_state: ReviewState::Approved,
                            revision: 1,
                        },
                    ];
                    let make_clip =
                        |id: &str,
                         scene_id: &str,
                         source_start: i64,
                         source_end: i64,
                         timeline_start: i64,
                         timeline_duration: i64| TimelineClip {
                            id: id.to_string(),
                            scene_id: Some(scene_id.to_string()),
                            turn_id: None,
                            media: MediaReference {
                                source_asset_id: Some(source_id.clone()),
                                render_artifact_id: None,
                            },
                            source_range: TimeRange::new(source_start, source_end)
                                .expect("valid fixture clip range"),
                            timeline_start_us: Microseconds(timeline_start),
                            timeline_duration_us: Microseconds(timeline_duration),
                            playback_rate: RationalRate::ONE,
                            gain_db_milli: 0,
                            muted: false,
                            crop: None,
                        };
                    let video_clips = vec![
                        make_clip(
                            "clip-video-one",
                            &scene_one,
                            0,
                            first_duration,
                            0,
                            first_duration,
                        ),
                        make_clip(
                            "clip-video-two",
                            &scene_two,
                            first_duration,
                            first_duration + second_duration,
                            second_timeline_start,
                            second_duration,
                        ),
                    ];
                    let audio_clips = vec![
                        make_clip(
                            "clip-audio-one",
                            &scene_one,
                            0,
                            first_duration,
                            0,
                            first_duration,
                        ),
                        make_clip(
                            "clip-audio-two",
                            &scene_two,
                            first_duration,
                            first_duration + second_duration,
                            second_timeline_start,
                            second_duration,
                        ),
                    ];
                    manifest.tracks = vec![
                        TimelineTrack {
                            id: "video-main".to_string(),
                            kind: TrackKind::Video,
                            clips: video_clips,
                            preserve_gaps: true,
                        },
                        TimelineTrack {
                            id: "audio-main".to_string(),
                            kind: TrackKind::Audio,
                            clips: audio_clips,
                            preserve_gaps: true,
                        },
                    ];
                    manifest.gaps = vec![
                        TimelineGap {
                            id: "gap-video".to_string(),
                            track_id: "video-main".to_string(),
                            range: TimeRange::new(first_duration, second_timeline_start)?,
                            reason: GapReason::Transition,
                            source_asset_id: None,
                            source_range: None,
                        },
                        TimelineGap {
                            id: "gap-audio".to_string(),
                            track_id: "audio-main".to_string(),
                            range: TimeRange::new(first_duration, second_timeline_start)?,
                            reason: GapReason::Editorial,
                            source_asset_id: None,
                            source_range: None,
                        },
                    ];
                    manifest.captions = vec![
                        CaptionCue {
                            id: "caption-one".to_string(),
                            range: TimeRange::new(first_duration / 8, first_duration / 2)?,
                            text: "First scene".to_string(),
                            style_id: "calm".to_string(),
                            speaker_id: Some("speaker-one".to_string()),
                            transcript_segment_id: None,
                            scene_id: Some(scene_one),
                        },
                        CaptionCue {
                            id: "caption-two".to_string(),
                            range: TimeRange::new(
                                second_timeline_start + second_duration / 8,
                                second_timeline_start + second_duration / 2,
                            )?,
                            text: "Second scene".to_string(),
                            style_id: "calm".to_string(),
                            speaker_id: Some("speaker-two".to_string()),
                            transcript_segment_id: None,
                            scene_id: Some(scene_two),
                        },
                    ];
                    Ok(())
                },
            )
            .expect("reviewed timeline commit");
        let reviewed_expectation = project_expectation(&reviewed).expect("reviewed expectation");
        let timeline_request = TimelineRenderRequest {
            project_id: video_project.clone(),
            expected_revision: reviewed_expectation.revision,
            expected_version_id: reviewed_expectation.version_id.clone(),
            profile: TimelineRenderProfile::Preview,
            caption_theme: CaptionTheme::Calm,
            portrait_layout: PortraitSourceLayout::CenterCrop,
            actor: "service-test".to_string(),
            variation: 0,
            include_title_cards: true,
            include_speaker_cards: true,
            burn_captions: true,
        };
        *workspace
            .service
            .single_render_test_failpoint
            .lock()
            .expect("timeline pre-publication failpoint") =
            Some(SingleRenderTestFailpoint::TimelineBeforeAtomicPublication);
        let timeline_job = workspace
            .service
            .queue_timeline_render(timeline_request.clone(), None)
            .expect("queue reviewed timeline");
        let before_atomic_error = workspace
            .service
            .wait_for_job(
                &timeline_job.job_id,
                &video_project,
                Duration::from_secs(180),
            )
            .expect_err("pre-transaction timeline crash is durable");
        assert_eq!(before_atomic_error.code, "video.job_failed");
        let after_precommit_crash = workspace
            .service
            .get_project(&video_project)
            .expect("project after pre-transaction timeline crash");
        assert_eq!(
            project_expectation(&after_precommit_crash).expect("precommit expectation"),
            reviewed_expectation,
            "the eliminated manifest-only boundary must publish neither revision nor output"
        );
        assert!(!after_precommit_crash
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| outputs.iter().any(|output| {
                output.get("job_id").and_then(Value::as_str) == Some(timeline_job.job_id.as_str())
            })));

        *workspace
            .service
            .single_render_test_failpoint
            .lock()
            .expect("timeline post-publication failpoint") =
            Some(SingleRenderTestFailpoint::TimelineAfterAtomicPublication);
        workspace
            .service
            .resume_job(&timeline_job.job_id, None)
            .expect("resume timeline into atomic publication");
        let after_atomic_error = workspace
            .service
            .wait_for_job(
                &timeline_job.job_id,
                &video_project,
                Duration::from_secs(180),
            )
            .expect_err("post-transaction timeline crash is durable");
        assert_eq!(after_atomic_error.code, "video.job_failed");
        let after_atomic_crash = workspace
            .service
            .get_project(&video_project)
            .expect("project after atomic timeline publication");
        let atomic_timeline_expectation =
            project_expectation(&after_atomic_crash).expect("atomic timeline expectation");
        assert_eq!(
            atomic_timeline_expectation.revision,
            reviewed_expectation.revision + 1
        );
        assert_eq!(
            after_atomic_crash
                .get("outputs")
                .and_then(Value::as_array)
                .expect("atomic timeline outputs")
                .iter()
                .filter(|output| {
                    output.get("job_id").and_then(Value::as_str)
                        == Some(timeline_job.job_id.as_str())
                })
                .count(),
            1,
            "manifest and one playable output commit in the same transaction"
        );
        workspace
            .service
            .resume_job(&timeline_job.job_id, None)
            .expect("resume already atomically published timeline");
        let first_timeline = workspace
            .service
            .wait_for_job(
                &timeline_job.job_id,
                &video_project,
                Duration::from_secs(30),
            )
            .expect("reviewed timeline post-commit result is adopted");
        assert_eq!(
            project_expectation(&first_timeline.project).expect("adopted timeline expectation"),
            atomic_timeline_expectation,
            "post-commit adoption must not create another artifact revision"
        );
        let first_timeline_manifest: VideoProjectManifest = serde_json::from_value(
            first_timeline
                .project
                .get("manifest")
                .cloned()
                .expect("timeline manifest"),
        )
        .expect("typed rendered timeline");
        for artifact in &first_timeline_manifest.render_artifacts {
            let path = workspace
                .service
                .resolve_managed_path(&artifact.managed_path)
                .expect("resolve private timeline artifact");
            assert_eq!(
                fs::metadata(path).expect("timeline artifact mode").mode() & 0o7777,
                PRIVATE_FILE_MODE,
                "ASS documents and cached timeline renders stay owner-only"
            );
        }
        let timeline_artifact = first_timeline_manifest
            .render_artifacts
            .iter()
            .find(|artifact| artifact.role == RenderArtifactRole::Preview)
            .expect("timeline preview artifact")
            .clone();
        let timeline_path = workspace
            .service
            .resolve_managed_path(&timeline_artifact.managed_path)
            .expect("managed timeline path");
        let timeline_probe = probe_media(
            &timeline_path,
            runtime.ffprobe.path.as_deref().expect("ffprobe path"),
        )
        .expect("playable assembled timeline");
        assert!((timeline_probe.duration_us - duration).abs() <= 300_000);
        let output_count = first_timeline
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .expect("outputs");
        let rerender_expectation =
            project_expectation(&first_timeline.project).expect("rerender expectation");
        let rerender = workspace
            .service
            .queue_timeline_render(
                TimelineRenderRequest {
                    project_id: video_project.clone(),
                    expected_revision: rerender_expectation.revision,
                    expected_version_id: rerender_expectation.version_id,
                    profile: TimelineRenderProfile::Preview,
                    caption_theme: CaptionTheme::Calm,
                    portrait_layout: PortraitSourceLayout::CenterCrop,
                    actor: "service-test".to_string(),
                    variation: 0,
                    include_title_cards: true,
                    include_speaker_cards: true,
                    burn_captions: true,
                },
                None,
            )
            .expect("queue cached reviewed timeline");
        let cached_timeline = workspace
            .service
            .wait_for_job(&rerender.job_id, &video_project, Duration::from_secs(180))
            .expect("cached timeline completes");
        assert_eq!(
            cached_timeline
                .project
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(output_count),
            "cache reuse must not duplicate the durable output record"
        );
        let cached_timeline_manifest: VideoProjectManifest = serde_json::from_value(
            cached_timeline
                .project
                .get("manifest")
                .cloned()
                .expect("cached timeline manifest"),
        )
        .expect("typed cached timeline");
        assert_eq!(
            cached_timeline_manifest
                .render_artifacts
                .iter()
                .filter(|artifact| artifact.cache_key == timeline_artifact.cache_key)
                .count(),
            1
        );
        let batch_base_expectation =
            project_expectation(&cached_timeline.project).expect("batch base expectation");
        let batch_base_revision = batch_base_expectation.revision;
        let batch = workspace
            .service
            .queue_timeline_render_batch(
                TimelineRenderBatchRequest {
                    base: TimelineRenderRequest {
                        project_id: video_project.clone(),
                        expected_revision: batch_base_expectation.revision,
                        expected_version_id: batch_base_expectation.version_id,
                        profile: TimelineRenderProfile::Final,
                        caption_theme: CaptionTheme::Calm,
                        portrait_layout: PortraitSourceLayout::CenterCrop,
                        actor: "service-test".to_string(),
                        variation: 0,
                        include_title_cards: true,
                        include_speaker_cards: true,
                        burn_captions: true,
                    },
                    variations: vec![0, 1, 2],
                },
                None,
            )
            .expect("queue frozen final variation batch");
        let batch_result = workspace
            .service
            .wait_for_job(&batch.job_id, &video_project, Duration::from_secs(300))
            .expect("final variation batch completes");
        let batch_expectation =
            project_expectation(&batch_result.project).expect("batch result expectation");
        assert_eq!(
            batch_expectation.revision,
            batch_base_revision + 1,
            "three variations must create exactly one artifact revision"
        );
        let current_batch_outputs = batch_result
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .expect("batch outputs")
            .iter()
            .filter(|output| {
                output.get("version_id").and_then(Value::as_str)
                    == Some(batch_expectation.version_id.as_str())
                    && output.get("job_id").and_then(Value::as_str) == Some(batch.job_id.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(current_batch_outputs.len(), 3);
        assert_eq!(
            current_batch_outputs
                .iter()
                .filter(|output| output.get("is_primary").and_then(Value::as_bool) == Some(true))
                .count(),
            1
        );
        assert_eq!(
            current_batch_outputs
                .iter()
                .filter(|output| output.get("kind").and_then(Value::as_str) == Some("variation"))
                .count(),
            2
        );
        assert_eq!(
            current_batch_outputs
                .iter()
                .map(|output| output
                    .get("sha256")
                    .and_then(Value::as_str)
                    .expect("output sha"))
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "variation layout/style choices must produce distinct playable masters"
        );
        let batch_manifest: VideoProjectManifest = serde_json::from_value(
            batch_result
                .project
                .get("manifest")
                .cloned()
                .expect("batch manifest"),
        )
        .expect("typed batch manifest");
        assert_eq!(
            batch_manifest
                .revision_history
                .iter()
                .filter(|record| record.revision as i64 > batch_base_revision)
                .count(),
            1
        );

        let replay = workspace
            .service
            .queue_timeline_render_batch(
                TimelineRenderBatchRequest {
                    base: TimelineRenderRequest {
                        project_id: video_project.clone(),
                        expected_revision: batch_expectation.revision,
                        expected_version_id: batch_expectation.version_id.clone(),
                        profile: TimelineRenderProfile::Final,
                        caption_theme: CaptionTheme::Calm,
                        portrait_layout: PortraitSourceLayout::CenterCrop,
                        actor: "service-test".to_string(),
                        variation: 0,
                        include_title_cards: true,
                        include_speaker_cards: true,
                        burn_captions: true,
                    },
                    variations: vec![0, 1, 2],
                },
                None,
            )
            .expect("queue cached variation batch replay");
        let replay = workspace
            .service
            .wait_for_job(&replay.job_id, &video_project, Duration::from_secs(120))
            .expect("cached batch replay completes");
        assert_eq!(
            replay.project.get("revision").and_then(Value::as_i64),
            Some(batch_expectation.revision),
            "cache/idempotency replay must not advance the artifact revision"
        );
        assert_eq!(
            replay
                .project
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            batch_result
                .project
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            "current-output identity adoption must not duplicate the batch"
        );

        // Variations 1 and 4 intentionally select the same effective layout;
        // with captions and cards disabled their FFmpeg commands produce
        // byte-identical media. They are still distinct semantic outputs and
        // every response/checkpoint must retain the exact stable Store ID.
        let identical_base =
            project_expectation(&replay.project).expect("identical-byte batch base");
        let published_progress = Arc::new(Mutex::new(Vec::<VideoServiceProgress>::new()));
        let progress_sink = Arc::clone(&published_progress);
        let progress_callback: ProgressCallback = Arc::new(move |progress| {
            if progress.phase == "published" {
                progress_sink
                    .lock()
                    .expect("published progress sink")
                    .push(progress);
            }
        });
        let identical_batch = workspace
            .service
            .queue_timeline_render_batch(
                TimelineRenderBatchRequest {
                    base: TimelineRenderRequest {
                        project_id: video_project.clone(),
                        expected_revision: identical_base.revision,
                        expected_version_id: identical_base.version_id,
                        profile: TimelineRenderProfile::Preview,
                        caption_theme: CaptionTheme::Calm,
                        portrait_layout: PortraitSourceLayout::CenterCrop,
                        actor: "service-test".to_string(),
                        variation: 0,
                        include_title_cards: false,
                        include_speaker_cards: false,
                        burn_captions: false,
                    },
                    variations: vec![1, 4],
                },
                Some(progress_callback),
            )
            .expect("queue identical-byte semantic variations");
        let identical_result = workspace
            .service
            .wait_for_job(
                &identical_batch.job_id,
                &video_project,
                Duration::from_secs(180),
            )
            .expect("identical-byte semantic variations complete");
        let identical_expectation =
            project_expectation(&identical_result.project).expect("identical batch expectation");
        let identical_outputs = identical_result
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .expect("identical batch outputs")
            .iter()
            .filter(|output| {
                output.get("version_id").and_then(Value::as_str)
                    == Some(identical_expectation.version_id.as_str())
                    && output.get("job_id").and_then(Value::as_str)
                        == Some(identical_batch.job_id.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(identical_outputs.len(), 2);
        assert_eq!(
            identical_outputs
                .iter()
                .filter_map(|output| output.get("sha256").and_then(Value::as_str))
                .collect::<BTreeSet<_>>()
                .len(),
            1,
            "the regression requires byte-identical encoded media"
        );
        let identical_ids = identical_outputs
            .iter()
            .map(|output| {
                let provenance = output.get("provenance").expect("variation provenance");
                let variation = provenance
                    .get("variation")
                    .and_then(Value::as_u64)
                    .and_then(|value| u16::try_from(value).ok())
                    .expect("variation discriminator");
                let render_key = provenance
                    .get("render_cache_key")
                    .and_then(Value::as_str)
                    .expect("render cache key");
                let expected_id = stable_output_id(
                    &video_project,
                    identical_expectation.revision,
                    "timeline-preview",
                    render_key,
                    variation,
                );
                assert_eq!(
                    output.get("id").and_then(Value::as_str),
                    Some(expected_id.as_str())
                );
                expected_id
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(identical_ids.len(), 2);
        let stages = workspace
            .service
            .store
            .list_video_stages(&video_project)
            .expect("identical batch stages");
        let checkpoint_ids = stages
            .iter()
            .filter(|stage| {
                stage.get("job_id").and_then(Value::as_str) == Some(identical_batch.job_id.as_str())
                    && stage.get("stage_key").and_then(Value::as_str) == Some("preview_render")
                    && stage.get("status").and_then(Value::as_str) == Some("completed")
            })
            .filter_map(|stage| {
                stage
                    .get("checkpoint")
                    .and_then(|checkpoint| checkpoint.get("output_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(checkpoint_ids, identical_ids);
        let progress = published_progress
            .lock()
            .expect("published progress")
            .last()
            .cloned()
            .expect("batch published progress");
        assert_eq!(progress.job_id, identical_batch.job_id);
        let progress_ids = progress
            .metrics
            .as_ref()
            .and_then(|metrics| metrics.get("outputs"))
            .and_then(Value::as_array)
            .expect("published progress outputs")
            .iter()
            .filter_map(|output| output.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        assert_eq!(progress_ids, identical_ids);

        let crash_base = project_expectation(&identical_result.project).expect("crash batch base");
        let crash_request = TimelineRenderBatchRequest {
            base: TimelineRenderRequest {
                project_id: video_project.clone(),
                expected_revision: crash_base.revision,
                expected_version_id: crash_base.version_id,
                profile: TimelineRenderProfile::Final,
                caption_theme: CaptionTheme::Calm,
                portrait_layout: PortraitSourceLayout::CenterCrop,
                actor: "service-test".to_string(),
                variation: 0,
                include_title_cards: true,
                include_speaker_cards: true,
                burn_captions: true,
            },
            variations: vec![3],
        };
        let crash_job = workspace
            .service
            .store
            .create_job(
                "video_render_timeline_batch_final",
                &serde_json::to_value(&crash_request).expect("durable crash batch"),
            )
            .expect("create crash-window batch job");
        workspace
            .service
            .perform_timeline_render_batch(
                &crash_job,
                &crash_request,
                &AtomicBool::new(false),
                None,
            )
            .expect("atomic batch commit before simulated crash");
        let after_atomic_commit = workspace
            .service
            .get_project(&video_project)
            .expect("project after atomic batch commit");
        let crash_committed_expectation =
            project_expectation(&after_atomic_commit).expect("atomic batch expectation");
        let crash_output_count = after_atomic_commit
            .get("outputs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .expect("output count after atomic batch");
        workspace
            .service
            .store
            .fail_job(&crash_job, "simulated crash after atomic commit")
            .expect("mark crash-window job resumable");
        let recovered = workspace
            .service
            .resume_job(&crash_job, None)
            .expect("resume atomically committed batch");
        let recovered = workspace
            .service
            .wait_for_job(&recovered.job_id, &video_project, Duration::from_secs(30))
            .expect("adopt complete batch after restart");
        assert_eq!(
            project_expectation(&recovered.project).expect("recovered expectation"),
            crash_committed_expectation,
            "post-commit recovery must not create a duplicate artifact revision"
        );
        assert_eq!(
            recovered
                .project
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(crash_output_count),
            "post-commit recovery must adopt, not duplicate, output rows"
        );

        let stale_base = project_expectation(&recovered.project).expect("stale batch base");
        let output_count_before_stale = recovered
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .expect("output count before stale batch");
        let advanced = Arc::new(AtomicBool::new(false));
        let advance_flag = Arc::clone(&advanced);
        let advance_service = Arc::clone(&workspace.service);
        let advance_project = video_project.clone();
        let callback: ProgressCallback = Arc::new(move |progress| {
            if progress.phase == "assembling"
                && advance_flag
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                advance_service
                    .commit_manifest_mutation(
                        &advance_project,
                        "concurrent-service-test",
                        "Advance editorial state while a frozen batch is rendering",
                        Some("draft"),
                        vec!["/name".to_string()],
                        BTreeSet::from([RevisionStage::PublishPackage]),
                        |manifest| {
                            manifest.name = "Imported reel concurrently revised".to_string();
                            Ok(())
                        },
                    )
                    .expect("concurrent editorial revision");
            }
        });
        let stale_batch = workspace
            .service
            .queue_timeline_render_batch(
                TimelineRenderBatchRequest {
                    base: TimelineRenderRequest {
                        project_id: video_project.clone(),
                        expected_revision: stale_base.revision,
                        expected_version_id: stale_base.version_id,
                        profile: TimelineRenderProfile::Preview,
                        caption_theme: CaptionTheme::Kinetic,
                        portrait_layout: PortraitSourceLayout::BlurPad,
                        actor: "service-test".to_string(),
                        variation: 0,
                        include_title_cards: false,
                        include_speaker_cards: false,
                        burn_captions: true,
                    },
                    variations: vec![0, 1],
                },
                Some(callback),
            )
            .expect("queue intentionally stale variation batch");
        let stale_error = workspace
            .service
            .wait_for_job(
                &stale_batch.job_id,
                &video_project,
                Duration::from_secs(180),
            )
            .expect_err("a concurrent edit must reject the entire frozen batch");
        assert_eq!(stale_error.code, "video.job_failed");
        assert!(advanced.load(Ordering::Acquire));
        let after_stale = workspace
            .service
            .get_project(&video_project)
            .expect("project after stale batch");
        assert_eq!(
            after_stale
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(output_count_before_stale),
            "a rejected frozen batch must publish zero partial outputs"
        );
        assert!(!after_stale
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| outputs.iter().any(|output| {
                output.get("job_id").and_then(Value::as_str) == Some(stale_batch.job_id.as_str())
            })));

        let cancellation_expectation =
            project_expectation(&after_stale).expect("cancellation expectation");
        let cancellable = workspace
            .service
            .queue_timeline_render(
                TimelineRenderRequest {
                    project_id: video_project.clone(),
                    expected_revision: cancellation_expectation.revision,
                    expected_version_id: cancellation_expectation.version_id,
                    profile: TimelineRenderProfile::Preview,
                    caption_theme: CaptionTheme::Kinetic,
                    portrait_layout: PortraitSourceLayout::BlurPad,
                    actor: "service-test".to_string(),
                    variation: 1,
                    include_title_cards: false,
                    include_speaker_cards: false,
                    burn_captions: true,
                },
                None,
            )
            .expect("queue cancellable timeline");
        assert!(workspace
            .service
            .cancel_job(&cancellable.job_id)
            .expect("cancel timeline"));
        let cancelled = workspace
            .service
            .wait_for_job(&cancellable.job_id, &video_project, Duration::from_secs(30))
            .expect_err("cancelled timeline must not complete");
        assert_eq!(cancelled.code, "video.cancelled");
        let after_cancel = workspace
            .service
            .get_project(&video_project)
            .expect("project after cancellation");
        assert!(!after_cancel
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| outputs.iter().any(|output| {
                output.get("job_id").and_then(Value::as_str) == Some(cancellable.job_id.as_str())
            })));

        let audio_fixture = workspace.root.join("fixture-podcast.wav");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:sample_rate=48000:duration=1",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&audio_fixture)
            .status()
            .expect("generate audio fixture");
        assert!(status.success());
        let audio_project = format!("project-{}", new_id());
        workspace.create_project(&audio_project, "Animated podcast");
        let import = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id: audio_project.clone(),
                    source_path: audio_fixture,
                    actor: "service-test".to_string(),
                    title: Some("A small local podcast".to_string()),
                },
                None,
            )
            .expect("queue audio import");
        let audio_imported = workspace
            .service
            .wait_for_job(&import.job_id, &audio_project, Duration::from_secs(120))
            .expect("audio import completes");
        let audio_render_base =
            project_expectation(&audio_imported.project).expect("audio render base");
        *workspace
            .service
            .single_render_test_failpoint
            .lock()
            .expect("portrait pre-publication failpoint") =
            Some(SingleRenderTestFailpoint::PortraitBeforeAtomicPublication);
        let render = workspace
            .service
            .queue_portrait_render(
                PortraitRenderRequest {
                    project_id: audio_project.clone(),
                    expected_revision: None,
                    expected_version_id: None,
                    source_asset_id: None,
                    profile: RenderProfile::Final,
                    layout: PortraitSourceLayout::Contain,
                    actor: "service-test".to_string(),
                    title: Some("Animated podcast".to_string()),
                    variation: 0,
                },
                None,
            )
            .expect("queue audio portrait render");
        let before_atomic_error = workspace
            .service
            .wait_for_job(&render.job_id, &audio_project, Duration::from_secs(180))
            .expect_err("pre-transaction portrait crash is durable");
        assert_eq!(before_atomic_error.code, "video.job_failed");
        let after_precommit_crash = workspace
            .service
            .get_project(&audio_project)
            .expect("project after pre-transaction portrait crash");
        assert_eq!(
            project_expectation(&after_precommit_crash).expect("portrait precommit expectation"),
            audio_render_base,
            "the eliminated portrait manifest-only boundary must publish no state"
        );
        assert!(!after_precommit_crash
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| outputs.iter().any(|output| {
                output.get("job_id").and_then(Value::as_str) == Some(render.job_id.as_str())
            })));

        *workspace
            .service
            .single_render_test_failpoint
            .lock()
            .expect("portrait post-publication failpoint") =
            Some(SingleRenderTestFailpoint::PortraitAfterAtomicPublication);
        workspace
            .service
            .resume_job(&render.job_id, None)
            .expect("resume portrait into atomic publication");
        let after_atomic_error = workspace
            .service
            .wait_for_job(&render.job_id, &audio_project, Duration::from_secs(180))
            .expect_err("post-transaction portrait crash is durable");
        assert_eq!(after_atomic_error.code, "video.job_failed");
        let after_atomic_crash = workspace
            .service
            .get_project(&audio_project)
            .expect("project after atomic portrait publication");
        let atomic_portrait_expectation =
            project_expectation(&after_atomic_crash).expect("atomic portrait expectation");
        assert_eq!(
            atomic_portrait_expectation.revision,
            audio_render_base.revision + 1
        );
        assert_eq!(
            after_atomic_crash
                .get("outputs")
                .and_then(Value::as_array)
                .expect("atomic portrait outputs")
                .iter()
                .filter(|output| {
                    output.get("job_id").and_then(Value::as_str) == Some(render.job_id.as_str())
                })
                .count(),
            1
        );
        workspace
            .service
            .resume_job(&render.job_id, None)
            .expect("resume already atomically published portrait");
        let rendered = workspace
            .service
            .wait_for_job(&render.job_id, &audio_project, Duration::from_secs(30))
            .expect("audio portrait post-commit result is adopted");
        assert_eq!(
            project_expectation(&rendered.project).expect("adopted portrait expectation"),
            atomic_portrait_expectation,
            "post-commit portrait adoption must not create another revision"
        );
        let primary = rendered
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .and_then(|outputs| {
                outputs
                    .iter()
                    .find(|output| output.get("is_primary").and_then(Value::as_bool) == Some(true))
            })
            .expect("primary final master");
        let primary_path = PathBuf::from(value_string(primary, "artifact_path").expect("path"));
        let primary_probe = probe_media(
            &primary_path,
            runtime.ffprobe.path.as_deref().expect("ffprobe path"),
        )
        .expect("playable final master");
        assert!(primary_probe.primary_video_stream.is_some());
        assert!(primary_probe.primary_audio_stream.is_some());

        let outputs_before_cancel = rendered
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .map(Vec::len)
            .expect("output count before package cancellation");
        let package_cancel_barrier = Arc::new(std::sync::Barrier::new(2));
        *workspace
            .service
            .package_test_barrier
            .lock()
            .expect("package cancellation barrier") = Some(Arc::clone(&package_cancel_barrier));
        let cancel_package_service = Arc::clone(&workspace.service);
        let cancel_package_project = audio_project.clone();
        let package_thread = thread::spawn(move || {
            cancel_package_service.export_publish_package(PublishPackageRequest {
                project_id: cancel_package_project,
                expected_revision: None,
                expected_version_id: None,
                destination_dir: None,
                actor: "service-test".to_string(),
            })
        });
        package_cancel_barrier.wait();
        let synchronous_package_job = workspace
            .service
            .cancellations
            .lock()
            .expect("registered synchronous package cancellation")
            .keys()
            .next()
            .cloned()
            .expect("synchronous package job is cancellation-addressable");
        assert!(workspace
            .service
            .cancel_job(&synchronous_package_job)
            .expect("cancel synchronous package"));
        package_cancel_barrier.wait();
        let package_cancelled = package_thread
            .join()
            .expect("synchronous package thread")
            .expect_err("cancelled synchronous package must not return success");
        assert_eq!(package_cancelled.code, "video.cancelled");
        let after_package_cancel = workspace
            .service
            .get_project(&audio_project)
            .expect("project after package cancellation");
        assert_eq!(
            after_package_cancel
                .get("outputs")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(outputs_before_cancel),
            "cancelling the direct package API before copy must publish no output"
        );
        assert!(!after_package_cancel
            .get("outputs")
            .and_then(Value::as_array)
            .is_some_and(|outputs| outputs.iter().any(|output| {
                output.get("job_id").and_then(Value::as_str)
                    == Some(synchronous_package_job.as_str())
            })));
        assert!(!workspace
            .service
            .cancellations
            .lock()
            .expect("package cancellation registration cleanup")
            .contains_key(&synchronous_package_job));

        let package = workspace
            .service
            .export_publish_package(PublishPackageRequest {
                project_id: audio_project.clone(),
                expected_revision: None,
                expected_version_id: None,
                destination_dir: None,
                actor: "service-test".to_string(),
            })
            .expect("publish package");
        let package_path = PathBuf::from(
            package
                .get("package_path")
                .and_then(Value::as_str)
                .expect("package path"),
        );
        let archive_path = PathBuf::from(
            package
                .get("archive_path")
                .and_then(Value::as_str)
                .expect("publish ZIP path"),
        );
        assert!(valid_package_directory(&package_path));
        validate_publish_zip(&archive_path, &package_path).expect("managed ZIP integrity");
        assert_eq!(
            fs::metadata(&package_path)
                .expect("package directory mode")
                .mode()
                & 0o7777,
            PRIVATE_DIRECTORY_MODE
        );
        for entry in fs::read_dir(&package_path).expect("private package members") {
            let path = entry.expect("package entry").path();
            assert_eq!(
                fs::metadata(path).expect("package member mode").mode() & 0o7777,
                PRIVATE_FILE_MODE,
                "managed package members stay owner-only"
            );
        }
        assert_eq!(
            fs::metadata(&archive_path)
                .expect("managed ZIP mode")
                .mode()
                & 0o7777,
            PRIVATE_FILE_MODE
        );
        assert_eq!(
            package
                .get("output")
                .and_then(|output| output.get("mime_type"))
                .and_then(Value::as_str),
            Some("application/zip")
        );
        assert_eq!(
            package
                .get("output")
                .and_then(|output| output.get("artifact_path"))
                .and_then(Value::as_str),
            archive_path.to_str()
        );
        let archive_sha = sha256_file(&archive_path).expect("publish ZIP checksum");
        let extracted_path = workspace.root.join("extracted-publish-package");
        fs::create_dir(&extracted_path).expect("ZIP extraction directory");
        let unzip = Path::new("/usr/bin/unzip");
        assert!(unzip.is_file(), "real package smoke test requires unzip");
        let extraction = Command::new(unzip)
            .args(["-qq", "-o"])
            .arg(&archive_path)
            .arg("-d")
            .arg(&extracted_path)
            .status()
            .expect("extract publish ZIP");
        assert!(
            extraction.success(),
            "publish ZIP must be externally extractable"
        );
        validate_package_directory(&extracted_path).expect("extracted package integrity");
        assert!(
            package_directories_equal(&package_path, &extracted_path, None)
                .expect("compare extracted hashes"),
            "every extracted ZIP member must match the managed package"
        );
        let package_manifest: Value = serde_json::from_slice(
            &fs::read(package_path.join("package-manifest.json")).expect("package manifest"),
        )
        .expect("valid package JSON");
        assert_eq!(
            package_manifest.get("kind").and_then(Value::as_str),
            Some("soundar_publish_package")
        );
        let package_project = workspace
            .service
            .get_project(&audio_project)
            .expect("package project");
        let package_expectation =
            project_expectation(&package_project).expect("package expectation");
        let resumable_request = PublishPackageRequest {
            project_id: audio_project.clone(),
            expected_revision: Some(package_expectation.revision),
            expected_version_id: Some(package_expectation.version_id),
            destination_dir: None,
            actor: "service-test".to_string(),
        };
        let resumable_job = workspace
            .service
            .store
            .create_job(
                "video_publish_package",
                &serde_json::to_value(&resumable_request).expect("durable package request"),
            )
            .expect("durable resumable package job");
        workspace
            .service
            .store
            .start_job(&resumable_job)
            .expect("start interrupted package job");
        workspace
            .service
            .store
            .fail_job(&resumable_job, "simulated application restart")
            .expect("interrupt package job");
        let resumed = workspace
            .service
            .resume_job(&resumable_job, None)
            .expect("resume owning package workflow");
        assert_eq!(resumed.job_id, resumable_job);
        let resumed_result = workspace
            .service
            .wait_for_job(&resumed.job_id, &audio_project, Duration::from_secs(60))
            .expect("resumed package completes");
        assert_eq!(
            resumed_result.job.get("status").and_then(Value::as_str),
            Some("completed")
        );
        assert_eq!(
            sha256_file(&archive_path).expect("resumed ZIP checksum"),
            archive_sha,
            "resuming the same publish request must reuse the deterministic ZIP"
        );
        let managed_master = package_path.join("master.mp4");
        let managed_master_sha = sha256_file(&managed_master).expect("managed master checksum");
        let export_parent = workspace.root.join("exports");
        fs::create_dir(&export_parent).expect("export parent");
        let exported = workspace
            .service
            .export_publish_package(PublishPackageRequest {
                project_id: audio_project.clone(),
                expected_revision: None,
                expected_version_id: None,
                destination_dir: Some(export_parent),
                actor: "service-test".to_string(),
            })
            .expect("export independent package");
        let exported_path = PathBuf::from(
            exported
                .get("export_path")
                .and_then(Value::as_str)
                .expect("export path"),
        );
        let exported_master = exported_path.join("master.mp4");
        use std::os::unix::fs::MetadataExt as _;
        assert_ne!(
            fs::metadata(&managed_master)
                .expect("managed metadata")
                .ino(),
            fs::metadata(&exported_master)
                .expect("export metadata")
                .ino(),
            "user exports must never hard-link managed artifacts"
        );
        assert_eq!(
            fs::metadata(&exported_master)
                .expect("shareable exported master mode")
                .mode()
                & 0o7777,
            SHAREABLE_FILE_MODE
        );
        OpenOptions::new()
            .append(true)
            .open(&exported_master)
            .and_then(|mut file| file.write_all(b"user-edit"))
            .expect("mutate exported copy");
        assert_eq!(
            sha256_file(&managed_master).expect("managed master remains readable"),
            managed_master_sha,
            "editing an export must not mutate the managed master"
        );
        assert!(valid_package_directory(&package_path));
        assert!(!valid_package_directory(&exported_path));

        workspace
            .service
            .commit_manifest_mutation(
                &audio_project,
                "service-test",
                "Changed the publish title after the final render",
                Some("draft"),
                vec!["/name".to_string()],
                BTreeSet::from([RevisionStage::PublishPackage]),
                |manifest| {
                    manifest.name = "Animated podcast revised".to_string();
                    Ok(())
                },
            )
            .expect("advance editorial timeline beyond the master");
        let stale_error = workspace
            .service
            .export_publish_package(PublishPackageRequest {
                project_id: audio_project.clone(),
                expected_revision: None,
                expected_version_id: None,
                destination_dir: None,
                actor: "service-test".to_string(),
            })
            .expect_err("an older-version master must not be packaged for a revised timeline");
        assert_eq!(stale_error.code, "video.final_master_required");

        // Return editorial content to A after the A -> B revision. The render
        // cache is intentionally reusable, but Store output IDs and package
        // payloads must be scoped to this later canonical version.
        workspace
            .service
            .commit_manifest_mutation(
                &audio_project,
                "service-test",
                "Restored the original publish title",
                Some("draft"),
                vec!["/name".to_string()],
                BTreeSet::from([RevisionStage::PublishPackage]),
                |manifest| {
                    manifest.name = "Animated podcast".to_string();
                    Ok(())
                },
            )
            .expect("restore content-equivalent editorial state");
        let restored_project = workspace
            .service
            .get_project(&audio_project)
            .expect("restored project");
        let restored_expectation =
            project_expectation(&restored_project).expect("restored expectation");
        let restored_render = workspace
            .service
            .queue_portrait_render(
                PortraitRenderRequest {
                    project_id: audio_project.clone(),
                    expected_revision: Some(restored_expectation.revision),
                    expected_version_id: Some(restored_expectation.version_id.clone()),
                    source_asset_id: None,
                    profile: RenderProfile::Final,
                    layout: PortraitSourceLayout::Contain,
                    actor: "service-test".to_string(),
                    title: Some("Animated podcast".to_string()),
                    variation: 0,
                },
                None,
            )
            .expect("queue content-equivalent cached master");
        let restored_render = workspace
            .service
            .wait_for_job(
                &restored_render.job_id,
                &audio_project,
                Duration::from_secs(120),
            )
            .expect("content-equivalent cached master publishes on later version");
        let restored_render_expectation =
            project_expectation(&restored_render.project).expect("restored render expectation");
        assert_eq!(
            restored_render_expectation, restored_expectation,
            "cache reuse must not create an artifact-only revision"
        );
        let restored_master = restored_render
            .project
            .get("outputs")
            .and_then(Value::as_array)
            .and_then(|outputs| {
                outputs.iter().find(|output| {
                    output.get("version_id").and_then(Value::as_str)
                        == Some(restored_expectation.version_id.as_str())
                        && output.get("kind").and_then(Value::as_str) == Some("master")
                        && output.get("is_primary").and_then(Value::as_bool) == Some(true)
                })
            })
            .expect("later-version primary master");
        assert_eq!(
            restored_master.get("sha256").and_then(Value::as_str),
            primary.get("sha256").and_then(Value::as_str),
            "A -> B -> A must reuse the content-equivalent render bytes"
        );
        assert_ne!(
            restored_master.get("id").and_then(Value::as_str),
            primary.get("id").and_then(Value::as_str),
            "the later project version must receive its own stable output identity"
        );
        let restored_package = workspace
            .service
            .export_publish_package(PublishPackageRequest {
                project_id: audio_project.clone(),
                expected_revision: Some(restored_expectation.revision),
                expected_version_id: Some(restored_expectation.version_id.clone()),
                destination_dir: None,
                actor: "service-test".to_string(),
            })
            .expect("package content-equivalent later version");
        let restored_package_path = PathBuf::from(
            restored_package
                .get("package_path")
                .and_then(Value::as_str)
                .expect("restored package path"),
        );
        assert_ne!(
            restored_package_path, package_path,
            "packages embedding different canonical versions must never share a cache directory"
        );
        let restored_package_manifest: Value = serde_json::from_slice(
            &fs::read(restored_package_path.join("package-manifest.json"))
                .expect("restored package manifest"),
        )
        .expect("valid restored package manifest");
        let restored_version_sha = restored_render
            .project
            .get("version")
            .and_then(|version| version.get("sha256"))
            .and_then(Value::as_str)
            .expect("restored version sha");
        assert_eq!(
            restored_package_manifest
                .pointer("/project/version/id")
                .and_then(Value::as_str),
            Some(restored_expectation.version_id.as_str())
        );
        assert_eq!(
            restored_package_manifest
                .pointer("/project/version/sha256")
                .and_then(Value::as_str),
            Some(restored_version_sha)
        );
        assert_eq!(
            restored_package_manifest
                .pointer("/master/output_id")
                .and_then(Value::as_str),
            restored_master.get("id").and_then(Value::as_str)
        );
        assert_ne!(
            restored_package_manifest
                .pointer("/project/version/id")
                .and_then(Value::as_str),
            package_manifest
                .pointer("/project/version/id")
                .and_then(Value::as_str),
            "the later ZIP must not reuse stale embedded version metadata"
        );
        let restored_timeline: VideoProjectManifest = serde_json::from_slice(
            &fs::read(restored_package_path.join("timeline-manifest.json"))
                .expect("restored timeline manifest"),
        )
        .expect("typed restored timeline manifest");
        assert_eq!(
            i64::try_from(restored_timeline.revision).ok(),
            Some(restored_expectation.revision)
        );
    }

    #[test]
    fn managed_storage_is_private_and_user_exports_remain_independent() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Private storage contract");
        let project_directory = workspace
            .service
            .project_dir(&project_id)
            .expect("private project directory");
        let cache_key = sha256_bytes(b"private-mode-fixture");
        let managed_file = workspace
            .service
            .cache_path("mode-test", &cache_key, "json")
            .expect("private cache path");
        write_new_file(&managed_file, br#"{"private":true}"#).expect("private managed file");

        for directory in [
            workspace.service.video_root.as_path(),
            project_directory.as_path(),
            managed_file.parent().expect("cache parent"),
        ] {
            assert_eq!(
                fs::metadata(directory).expect("directory metadata").mode() & 0o7777,
                PRIVATE_DIRECTORY_MODE,
                "managed directories must be owner-only: {}",
                directory.display()
            );
        }
        assert_eq!(
            fs::metadata(&managed_file)
                .expect("managed file metadata")
                .mode()
                & 0o7777,
            PRIVATE_FILE_MODE
        );

        // Opening an older app-owned artifact repairs ambient permissions at
        // the same service boundary used by Projects, History and the agent.
        fs::set_permissions(&managed_file, fs::Permissions::from_mode(0o644))
            .expect("loosen fixture permissions");
        workspace
            .service
            .resolve_absolute_managed_path(&managed_file)
            .expect("managed resolution repairs permissions");
        assert_eq!(
            fs::metadata(&managed_file)
                .expect("repaired metadata")
                .mode()
                & 0o7777,
            PRIVATE_FILE_MODE
        );

        let user_copy = workspace.root.join("shareable-copy.json");
        copy_file_verified(
            &managed_file,
            &user_copy,
            CopiedFileVisibility::UserShareable,
            &AtomicBool::new(false),
        )
        .expect("shareable independent copy");
        assert_eq!(
            fs::metadata(&user_copy).expect("shareable metadata").mode() & 0o7777,
            SHAREABLE_FILE_MODE
        );
        assert_ne!(
            fs::metadata(&managed_file).expect("managed inode").ino(),
            fs::metadata(&user_copy).expect("export inode").ino(),
            "shareable copies must not alias managed artifacts"
        );

        let external = workspace.root.join("external-hardlink-source.json");
        fs::write(&external, br#"{"external":true}"#).expect("external fixture");
        fs::set_permissions(&external, fs::Permissions::from_mode(0o644))
            .expect("external fixture permissions");
        let aliased_managed = workspace
            .service
            .cache_path(
                "mode-test",
                &sha256_bytes(b"external-hardlink-alias"),
                "json",
            )
            .expect("managed alias path");
        fs::hard_link(&external, &aliased_managed).expect("hard-link attack fixture");
        let error = secure_managed_file(&aliased_managed)
            .expect_err("multi-link managed artifacts must fail closed");
        assert_eq!(error.code, "video.unsafe_artifact_path");
        assert_eq!(
            fs::metadata(&external)
                .expect("external mode remains visible")
                .mode()
                & 0o7777,
            0o644,
            "rejecting a managed hard-link must not chmod its external alias"
        );

        let outside = workspace.root.join("outside-directory");
        fs::create_dir(&outside).expect("outside directory");
        let unsafe_link = workspace.service.video_root.join("unsafe-link");
        std::os::unix::fs::symlink(&outside, &unsafe_link).expect("managed symlink fixture");
        let error = secure_managed_directory_path(&workspace.service.video_root, &unsafe_link)
            .expect_err("managed directory symlinks must fail closed");
        assert_eq!(error.code, "video.unsafe_storage_path");
    }

    #[test]
    fn source_disk_duration_and_package_limits_fail_before_expensive_work() {
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Resource bounds");
        let oversized = workspace.root.join("oversized-source.mp4");
        File::create(&oversized)
            .and_then(|file| file.set_len(MAX_SOURCE_BYTES + 1))
            .expect("sparse oversized fixture");
        let error = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id,
                    source_path: oversized,
                    actor: "service-test".to_string(),
                    title: None,
                },
                None,
            )
            .expect_err("oversized source must fail before FFprobe or job creation");
        assert_eq!(error.code, "video.source_too_large");

        let error = ensure_disk_capacity(
            &workspace.service.video_root,
            u64::MAX,
            "impossible-test-allocation",
        )
        .expect_err("impossible disk reservation must fail");
        assert_eq!(error.code, "video.insufficient_disk_space");
        assert!(error.retryable);

        assert_eq!(
            render_timeout(Some(1_000_000), RenderProfile::Proxy).expect("short proxy deadline"),
            Duration::from_secs(5 * 60)
        );
        assert_eq!(
            render_timeout(Some(1_000_000), RenderProfile::Final).expect("short final deadline"),
            Duration::from_secs(10 * 60)
        );
        assert_eq!(
            render_timeout(Some(MAX_MEDIA_DURATION_US), RenderProfile::Final)
                .expect("bounded six-hour deadline"),
            MAX_RENDER_DURATION
        );
        assert_eq!(
            render_timeout(Some(MAX_MEDIA_DURATION_US + 1), RenderProfile::Preview)
                .expect_err("overlong render must fail")
                .code,
            "video.duration_out_of_range"
        );
        assert_eq!(
            validate_package_aggregate_bytes(MAX_PACKAGE_AGGREGATE_BYTES + 1)
                .expect_err("oversized package must fail")
                .code,
            "video.package_too_large"
        );
        assert_eq!(
            bounded_publish_package_bytes(
                MAX_PACKAGE_AGGREGATE_BYTES - PACKAGE_METADATA_RESERVE_BYTES
            )
            .expect("master with metadata reserve fits"),
            MAX_PACKAGE_AGGREGATE_BYTES
        );
        assert_eq!(
            bounded_publish_package_bytes(
                MAX_PACKAGE_AGGREGATE_BYTES - PACKAGE_METADATA_RESERVE_BYTES + 1
            )
            .expect_err("metadata reserve is enforced before package copying")
            .code,
            "video.package_too_large"
        );
    }

    #[test]
    fn local_copy_is_descriptor_bound_and_rejects_growth_after_admission() {
        let workspace = TestWorkspace::new();
        let source = workspace.root.join("mutable-local-source.bin");
        let replacement = workspace.root.join("replacement-local-source.bin");
        fs::write(&source, b"validated-source-bytes").expect("validated source");
        fs::write(&replacement, b"different-path-bytes").expect("replacement source");
        let identity = LocalSourceIdentity::from_metadata(
            &fs::metadata(&source).expect("validated source metadata"),
        );
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&source)
            .expect("open validated source once");
        fs::rename(&replacement, &source).expect("swap the external path after admission");
        let copied = workspace.root.join("descriptor-bound-copy.bin");
        copy_file_cancelable(opened, identity, &copied, &AtomicBool::new(false), |_| {})
            .expect("copy remains bound to the admitted descriptor");
        assert_eq!(
            fs::read(&copied).expect("descriptor copy"),
            b"validated-source-bytes"
        );

        let growing = workspace.root.join("growing-local-source.bin");
        fs::write(&growing, b"small").expect("growing source");
        let growing_identity = LocalSourceIdentity::from_metadata(
            &fs::metadata(&growing).expect("growing source metadata"),
        );
        let growing_opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&growing)
            .expect("open growing source");
        OpenOptions::new()
            .append(true)
            .open(&growing)
            .and_then(|mut file| file.write_all(b"-grew-after-admission"))
            .expect("grow source after admission");
        let rejected_copy = workspace.root.join("rejected-growing-copy.bin");
        let error = copy_file_cancelable(
            growing_opened,
            growing_identity,
            &rejected_copy,
            &AtomicBool::new(false),
            |_| {},
        )
        .expect_err("growth beyond the admitted identity must fail closed");
        assert_eq!(error.code, "video.source_changed");
        assert!(!rejected_copy.exists());
        assert!(
            with_disk_headroom(MAX_SOURCE_BYTES, 1) >= MAX_SOURCE_BYTES,
            "mutable local imports reserve the full 8 GiB copy budget, not the stale stat size"
        );
    }

    #[test]
    fn long_publication_copy_observes_cancellation_and_removes_partial_output() {
        let workspace = TestWorkspace::new();
        let source = workspace.root.join("copy-source.bin");
        let mut file = File::create(&source).expect("copy source");
        file.write_all(&vec![0x5a; 2 * 1024 * 1024])
            .expect("copy fixture bytes");
        file.sync_all().expect("copy source sync");
        let destination = workspace.root.join("copy-destination.bin");
        let cancel = AtomicBool::new(true);
        let error = copy_file_verified(
            &source,
            &destination,
            CopiedFileVisibility::UserShareable,
            &cancel,
        )
        .expect_err("cancelled package copy must stop");
        assert_eq!(error.code, "video.cancelled");
        assert!(
            !destination.exists(),
            "a cancelled publication may not leave a partial file"
        );
    }

    #[test]
    fn captured_command_bounds_hot_output_and_descendant_pipe_drain() {
        let shell = Path::new("/bin/sh");
        if !shell.is_file() {
            eprintln!("Skipping bounded command test because /bin/sh is unavailable");
            return;
        }
        let hot_output = [
            OsString::from("-c"),
            OsString::from("yes x | head -c 16777216"),
        ];
        let error = run_captured_command(
            shell,
            &hot_output,
            Duration::from_secs(5),
            None,
            1_024,
            None,
            None,
        )
        .expect_err("oversized helper output must stay bounded and fail closed");
        assert_eq!(error.code, "video.command_output_too_large");

        let inherited_pipe = [OsString::from("-c"), OsString::from("(sleep 30) & exit 0")];
        let started = Instant::now();
        let error = run_captured_command(
            shell,
            &inherited_pipe,
            Duration::from_secs(10),
            None,
            1_024,
            None,
            None,
        )
        .expect_err("a descendant-held pipe must not hang the service");
        assert_eq!(error.code, "video.command_pipe_open");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "pipe drain and process-group cleanup must remain bounded"
        );

        let proxy_url = "http://127.0.0.1:43117/fixture-token";
        let inspect_proxy_env = [
            OsString::from("-c"),
            OsString::from(
                "printf '%s|%s|%s|%s|%s' \"$HTTP_PROXY\" \"$HTTPS_PROXY\" \"$ALL_PROXY\" \"${NO_PROXY-unset}\" \"${no_proxy-unset}\"",
            ),
        ];
        let captured = run_captured_command(
            shell,
            &inspect_proxy_env,
            Duration::from_secs(5),
            None,
            1_024,
            Some(proxy_url),
            None,
        )
        .expect("sanitized proxy environment");
        assert!(captured.status.success());
        assert_eq!(
            String::from_utf8(captured.stdout).expect("UTF-8 proxy fixture"),
            format!("{proxy_url}|{proxy_url}|{proxy_url}|unset|unset")
        );

        let workspace = TestWorkspace::new();
        let quota_dir = workspace.root.join("watched-download");
        fs::create_dir(&quota_dir).expect("watched download directory");
        let quota = CommandOutputQuota {
            directory: quota_dir.clone(),
            prefix: ".download-quota".to_string(),
            max_file_bytes: 128 * 1024,
            max_aggregate_bytes: 128 * 1024,
        };
        let streaming_writer = [
            OsString::from("-c"),
            OsString::from("yes x > \"$1/.download-quota.bin\""),
            OsString::from("soundar-quota-test"),
            quota_dir.as_os_str().to_os_string(),
        ];
        let started = Instant::now();
        let error = run_captured_command(
            shell,
            &streaming_writer,
            Duration::from_secs(10),
            None,
            1_024,
            None,
            Some(&quota),
        )
        .expect_err("a streaming helper must be killed at its watched-prefix quota");
        assert_eq!(error.code, "video.source_too_large");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn storage_reservations_prevent_concurrent_overcommit_and_release_on_drop() {
        let workspace = TestWorkspace::new();
        let available = available_disk_bytes(&workspace.service.video_root)
            .expect("available storage for reservation fixture");
        let first_bytes = (available / 2).max(1);
        let second_bytes = available.saturating_sub(first_bytes).saturating_add(1);
        let first = workspace
            .service
            .reserve_storage(
                "reservation-one",
                &workspace.service.video_root,
                first_bytes,
                "reservation-test",
            )
            .expect("first reservation");
        let error = workspace
            .service
            .reserve_storage(
                "reservation-two",
                &workspace.service.video_root,
                second_bytes,
                "reservation-test",
            )
            .err()
            .expect("concurrent reservations may not overcommit one device");
        assert_eq!(error.code, "video.insufficient_disk_space");
        drop(first);
        workspace
            .service
            .reserve_storage(
                "reservation-two",
                &workspace.service.video_root,
                second_bytes,
                "reservation-test",
            )
            .expect("RAII release restores storage capacity");
    }

    #[test]
    fn failed_download_cleanup_removes_only_job_owned_artifacts() {
        let workspace = TestWorkspace::new();
        let source_dir = workspace.root.join("download-cleanup");
        fs::create_dir(&source_dir).expect("download cleanup directory");
        let prefix = ".download-owned";
        let partial = source_dir.join(format!("{prefix}.mp4"));
        File::create(&partial)
            .and_then(|file| file.set_len(MAX_SOURCE_BYTES + 1))
            .expect("oversized sparse download fixture");
        assert_eq!(
            validate_source_size(fs::metadata(&partial).expect("partial metadata").len())
                .expect_err("post-download quota must reject oversized media")
                .code,
            "video.source_too_large"
        );
        let published = source_dir.join("source-owned.mp4");
        fs::write(&published, b"published").expect("published fixture");
        let unrelated = source_dir.join("keep.mp4");
        fs::write(&unrelated, b"unrelated").expect("unrelated fixture");

        cleanup_failed_download(&source_dir, prefix, Some(&published));

        assert!(!partial.exists());
        assert!(!published.exists());
        assert_eq!(
            fs::read(&unrelated).expect("unrelated retained"),
            b"unrelated"
        );
    }

    #[test]
    fn package_aggregate_preflight_rejects_sparse_member_before_hashing() {
        let workspace = TestWorkspace::new();
        let package = workspace.root.join("oversized-package");
        fs::create_dir(&package).expect("package directory");
        File::create(package.join("master.mp4"))
            .and_then(|file| file.set_len(MAX_PACKAGE_AGGREGATE_BYTES + 1))
            .expect("oversized sparse package fixture");
        let started = Instant::now();
        let error = package_member_inventory(&package, None)
            .expect_err("aggregate cap must run before checksum work");
        assert_eq!(error.code, "video.package_too_large");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "metadata preflight must reject sparse oversized packages immediately"
        );

        let archive = workspace.root.join("oversized-package.zip");
        File::create(&archive)
            .and_then(|file| file.set_len(MAX_PACKAGE_ARCHIVE_BYTES + 1))
            .expect("oversized sparse archive fixture");
        let empty_package = workspace.root.join("empty-package");
        fs::create_dir(&empty_package).expect("empty package fixture");
        let started = Instant::now();
        let error = write_publish_zip_atomic(&empty_package, &archive, &AtomicBool::new(false))
            .expect_err("archive cap must run before package inventory hashing");
        assert_eq!(error.code, "video.package_too_large");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn render_staging_cleanup_covers_cancellation_and_descendant_pipes() {
        let shell = Path::new("/bin/sh");
        if !shell.is_file() {
            eprintln!("Skipping render cleanup test because /bin/sh is unavailable");
            return;
        }
        let workspace = TestWorkspace::new();
        let project_id = format!("project-{}", new_id());
        workspace.create_project(&project_id, "Render cleanup");
        let output = workspace
            .service
            .cache_path("render-cleanup", &sha256_bytes(b"cancelled-render"), "mp4")
            .expect("render staging output");
        let plan = RenderCommandPlan {
            profile: RenderProfile::Proxy,
            workload_class: RenderWorkloadClass::Medium,
            primary: RenderCommand {
                program: shell.to_path_buf(),
                args: vec![
                    OsString::from("-c"),
                    OsString::from("printf partial > \"$1\"; sleep 30"),
                    OsString::from("soundar-render-test"),
                    output.as_os_str().to_os_string(),
                ],
                output: output.clone(),
                encoder: VideoEncoder::Libx264,
                emits_progress: false,
            },
            software_fallback: None,
        };
        let service = Arc::clone(&workspace.service);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_project = project_id.clone();
        let worker = thread::spawn(move || {
            service.execute_render_plan(
                "render-cleanup-job",
                &worker_project,
                &plan,
                Some(1_000_000),
                0.0,
                1.0,
                &worker_cancel,
                None,
            )
        });
        let wait_deadline = Instant::now() + Duration::from_secs(2);
        while !output.exists() && Instant::now() < wait_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(output.exists(), "fixture must create a staged render");
        cancel.store(true, Ordering::Release);
        let error = worker
            .join()
            .expect("render cleanup worker")
            .expect_err("cancelled render must stop");
        assert_eq!(error.code, "video.cancelled");
        assert!(!output.exists(), "cancelled render staging must be removed");

        let inherited_output = workspace
            .service
            .cache_path(
                "render-cleanup",
                &sha256_bytes(b"inherited-render-pipe"),
                "mp4",
            )
            .expect("inherited-pipe output");
        let inherited_plan = RenderCommandPlan {
            profile: RenderProfile::Proxy,
            workload_class: RenderWorkloadClass::Medium,
            primary: RenderCommand {
                program: shell.to_path_buf(),
                args: vec![
                    OsString::from("-c"),
                    OsString::from("(sleep 30 >&2) & printf partial > \"$1\"; exit 0"),
                    OsString::from("soundar-render-test"),
                    inherited_output.as_os_str().to_os_string(),
                ],
                output: inherited_output.clone(),
                encoder: VideoEncoder::Libx264,
                emits_progress: false,
            },
            software_fallback: None,
        };
        let started = Instant::now();
        let error = workspace
            .service
            .execute_render_plan(
                "render-pipe-job",
                &project_id,
                &inherited_plan,
                Some(1_000_000),
                0.0,
                1.0,
                &AtomicBool::new(false),
                None,
            )
            .expect_err("descendant-held render pipe must fail closed");
        assert_eq!(error.code, "video.render_pipe_open");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            !inherited_output.exists(),
            "failed render staging must be removed"
        );
    }
}

impl VideoStudioService {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn commit_manifest_mutation<F>(
        &self,
        project_id: &str,
        actor: &str,
        reason: &str,
        status: Option<&str>,
        changed_paths: Vec<String>,
        invalidated_stages: BTreeSet<RevisionStage>,
        mutate: F,
    ) -> ServiceResult<Value>
    where
        F: FnOnce(&mut VideoProjectManifest) -> ServiceResult<()>,
    {
        let current = self.get_project(project_id)?;
        let expectation = project_expectation(&current)?;
        self.commit_manifest_mutation_at(
            project_id,
            &expectation,
            actor,
            reason,
            status,
            changed_paths,
            invalidated_stages,
            mutate,
        )
    }

    /// Commits a generated artifact only if the timeline version used to
    /// produce it is still current. This is the publication CAS boundary:
    /// expensive work may finish after an edit, but it can never be attached
    /// to that newer edit by accident.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn commit_manifest_mutation_at<F>(
        &self,
        project_id: &str,
        expectation: &ProjectExpectation,
        actor: &str,
        reason: &str,
        status: Option<&str>,
        changed_paths: Vec<String>,
        invalidated_stages: BTreeSet<RevisionStage>,
        mutate: F,
    ) -> ServiceResult<Value>
    where
        F: FnOnce(&mut VideoProjectManifest) -> ServiceResult<()>,
    {
        self.commit_manifest_mutation_at_if_parent_active(
            project_id,
            expectation,
            actor,
            reason,
            status,
            changed_paths,
            invalidated_stages,
            None,
            mutate,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_manifest_mutation_at_if_parent_active<F>(
        &self,
        project_id: &str,
        expectation: &ProjectExpectation,
        actor: &str,
        reason: &str,
        status: Option<&str>,
        changed_paths: Vec<String>,
        invalidated_stages: BTreeSet<RevisionStage>,
        required_active_job_id: Option<&str>,
        mutate: F,
    ) -> ServiceResult<Value>
    where
        F: FnOnce(&mut VideoProjectManifest) -> ServiceResult<()>,
    {
        let actor = require_text(actor, "video.invalid_actor", "An actor is required")?;
        let reason = require_text(
            reason,
            "video.invalid_revision",
            "A revision reason is required",
        )?;
        if changed_paths.is_empty() {
            return Err(VideoServiceError::new(
                "video.invalid_revision",
                "A manifest mutation must identify at least one changed path",
            ));
        }
        let lock = ProjectLock::acquire(self, project_id, actor)?;
        let current = self.get_project(project_id)?;
        ensure_project_matches(&current, expectation)?;
        let expected_revision = expectation.revision;
        let mut manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        if i64::try_from(manifest.revision).ok() != Some(expected_revision) {
            return Err(VideoServiceError::new(
                "video.revision_integrity_failed",
                "The stored manifest and project revision are not aligned",
            )
            .details(json!({
                "store_revision": expected_revision,
                "manifest_revision": manifest.revision,
            })));
        }
        mutate(&mut manifest)?;
        let next_revision = manifest.revision.checked_add(1).ok_or_else(|| {
            VideoServiceError::new(
                "video.revision_overflow",
                "The manifest revision could not be advanced",
            )
        })?;
        let parent_id = manifest
            .revision_history
            .last()
            .map(|record| record.id.clone());
        manifest.revision = next_revision;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: next_revision,
            parent_id,
            actor: actor.to_string(),
            reason: reason.to_string(),
            changed_paths,
            invalidated_stages,
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict()?;
        let manifest_value = serde_json::to_value(&manifest).map_err(json_error)?;
        let result = if let Some(required_active_job_id) = required_active_job_id {
            self.store.commit_video_manifest_if_job_active(
                project_id,
                expected_revision,
                &manifest_value,
                actor,
                reason,
                &lock.token,
                status,
                required_active_job_id,
            )
        } else {
            self.store.commit_video_manifest(
                project_id,
                expected_revision,
                &manifest_value,
                actor,
                reason,
                &lock.token,
                status,
            )
        }
        .map_err(VideoServiceError::store)?;
        let result_revision = value_i64(&result, "revision")?;
        let returned_manifest_revision = result
            .get("manifest")
            .and_then(|value| value.get("revision"))
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_store_shape("manifest.revision"))?;
        if result_revision != expected_revision + 1
            || returned_manifest_revision != next_revision
            || i64::try_from(returned_manifest_revision).ok() != Some(result_revision)
        {
            return Err(VideoServiceError::new(
                "video.revision_integrity_failed",
                "The committed project and manifest revisions diverged",
            ));
        }
        drop(lock);
        Ok(result)
    }

    fn acquire_resources<'a>(
        &'a self,
        job_id: &str,
        project_id: &str,
        request: ResourceRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<SchedulerLease<'a>> {
        let requires_shared_gpu = request.vram_mb > 0
            || request.nvenc_sessions > 0
            || matches!(request.class, ResourceClass::Exclusive);
        let shared_request = SharedGpuAdmissionRequest {
            job_id: job_id.to_string(),
            project_id: project_id.to_string(),
            resource_class: request.class,
            requested_vram_mb: request.vram_mb,
            requested_nvenc_sessions: request.nvenc_sessions,
            exclusive: matches!(request.class, ResourceClass::Exclusive),
        };
        let mut last_notice = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .unwrap_or_else(Instant::now);
        loop {
            self.ensure_not_cancelled(cancel)?;
            let outcome = self
                .scheduler
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_acquire(job_id.to_string(), request)?;
            match outcome {
                AdmissionOutcome::Admitted(_) => {
                    let mut lease = SchedulerLease {
                        scheduler: &self.scheduler,
                        job_id: job_id.to_string(),
                        shared_gpu_lease: None,
                    };
                    let Some(gate) = self
                        .gpu_admission_gate
                        .as_ref()
                        .filter(|_| requires_shared_gpu)
                    else {
                        return Ok(lease);
                    };
                    // No local scheduler mutex is held across this external
                    // call. Normal backpressure releases the local lease before
                    // sleeping, preventing lock inversion and capacity hoarding.
                    self.ensure_not_cancelled(cancel)?;
                    match gate.try_acquire(&shared_request)? {
                        SharedGpuAdmissionOutcome::Admitted(shared) => {
                            lease.shared_gpu_lease = Some(shared);
                            return Ok(lease);
                        }
                        SharedGpuAdmissionOutcome::Waiting(wait) => {
                            drop(lease);
                            if last_notice.elapsed() >= Duration::from_secs(1) {
                                emit_progress(
                                    callback,
                                    job_id,
                                    project_id,
                                    "waiting_for_gpu",
                                    0.01,
                                    "Waiting for shared GPU capacity",
                                    None,
                                    Some(json!({
                                        "request": shared_request,
                                        "reason": wait.reason,
                                        "details": wait.details,
                                    })),
                                );
                                last_notice = Instant::now();
                            }
                            self.wait_for_resource_retry(
                                cancel,
                                Duration::from_millis(wait.retry_after_ms.clamp(10, 250)),
                            )?;
                        }
                    }
                }
                AdmissionOutcome::Waiting { blocks } => {
                    if last_notice.elapsed() >= Duration::from_secs(1) {
                        emit_progress(
                            callback,
                            job_id,
                            project_id,
                            "waiting_for_resources",
                            0.01,
                            "Waiting for safe local compute capacity",
                            None,
                            Some(json!({ "blocks": blocks })),
                        );
                        last_notice = Instant::now();
                    }
                    self.wait_for_resource_retry(cancel, RESOURCE_POLL_INTERVAL)?;
                }
            }
        }
    }

    fn wait_for_resource_retry(
        &self,
        cancel: &AtomicBool,
        duration: Duration,
    ) -> ServiceResult<()> {
        let deadline = Instant::now() + duration;
        loop {
            self.ensure_not_cancelled(cancel)?;
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            thread::sleep((deadline - now).min(Duration::from_millis(25)));
        }
    }

    fn reserve_storage<'a>(
        &'a self,
        reservation_id: impl Into<String>,
        path: &Path,
        required_bytes: u64,
        operation: &str,
    ) -> ServiceResult<StorageLease<'a>> {
        let reservation_id = reservation_id.into();
        let device_id = fs::metadata(path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.storage_unavailable",
                    "The storage reservation path could not be inspected",
                    error,
                )
            })?
            .dev();
        let available_bytes = available_disk_bytes(path)?;
        let mut state = self
            .storage_reservations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active.contains_key(&reservation_id) {
            return Err(VideoServiceError::new(
                "video.storage_reservation_conflict",
                "This video task already owns a storage reservation",
            )
            .details(json!({ "reservation_id": reservation_id })));
        }
        let reserved_bytes = state
            .active
            .values()
            .filter(|reservation| reservation.device_id == device_id)
            .fold(0_u64, |total, reservation| {
                total.saturating_add(reservation.bytes)
            });
        let committed_bytes = reserved_bytes.saturating_add(required_bytes);
        if available_bytes < committed_bytes {
            return Err(VideoServiceError::new(
                "video.insufficient_disk_space",
                "Video Studio needs more unreserved disk space before this task can start",
            )
            .retryable(true)
            .details(json!({
                "operation": operation,
                "path": path,
                "required_bytes": required_bytes,
                "already_reserved_bytes": reserved_bytes,
                "available_bytes": available_bytes,
            })));
        }
        state.active.insert(
            reservation_id.clone(),
            StorageReservation {
                device_id,
                bytes: required_bytes,
            },
        );
        Ok(StorageLease {
            reservations: &self.storage_reservations,
            id: reservation_id,
        })
    }

    fn ensure_not_cancelled(&self, cancel: &AtomicBool) -> ServiceResult<()> {
        if cancel.load(Ordering::Acquire) {
            Err(VideoServiceError::cancelled())
        } else {
            Ok(())
        }
    }

    fn durable_job_created_at(&self, job_id: &str) -> ServiceResult<String> {
        let job = self
            .store
            .get_job(job_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.job_not_found",
                    "The durable video job could not be reloaded",
                )
            })?;
        let created_at = value_string(&job, "created_at")?;
        normalize_utc_timestamp(&created_at)
    }

    #[cfg(test)]
    fn trigger_single_render_test_failpoint(
        &self,
        expected: SingleRenderTestFailpoint,
    ) -> ServiceResult<()> {
        let mut failpoint = self
            .single_render_test_failpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failpoint.as_ref() != Some(&expected) {
            return Ok(());
        }
        failpoint.take();
        Err(VideoServiceError::new(
            "video.test_crash",
            "Simulated process stop at the single-render publication boundary",
        ))
    }

    /// Recognizes only the exact single-render transaction owned by this
    /// durable job. Manifest and output publication are atomic, but the
    /// process can still stop before `complete_job`; a resumed worker adopts
    /// that already-committed result instead of rejecting its original frozen
    /// expectation or creating another artifact revision.
    #[allow(clippy::too_many_arguments)]
    fn recover_single_render_output(
        &self,
        project: &Value,
        job_id: &str,
        expectation: &ProjectExpectation,
        actor: &str,
        reason: &str,
        invalidated_stage: RevisionStage,
        request_sha256: &str,
        semantic_role: &str,
        variation: u16,
        expected_kind: &str,
        expected_primary: bool,
        render_key_field: &str,
        caption_key_field: Option<&str>,
        cancel: &AtomicBool,
    ) -> ServiceResult<Option<Value>> {
        let current = project_expectation(project)?;
        if current.revision != expectation.revision.saturating_add(1) {
            return Ok(None);
        }
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        let Some(record) = manifest.revision_history.last() else {
            return Ok(None);
        };
        if i64::try_from(record.revision).ok() != Some(current.revision)
            || record.actor != actor
            || record.reason != reason
            || record.changed_paths != ["/render_artifacts".to_string()]
            || record.invalidated_stages != BTreeSet::from([invalidated_stage])
        {
            return Ok(None);
        }
        let outputs = project
            .get("outputs")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_store_shape("outputs"))?;
        let matching = outputs
            .iter()
            .filter(|output| {
                output.get("version_id").and_then(Value::as_str)
                    == Some(current.version_id.as_str())
                    && output.get("job_id").and_then(Value::as_str) == Some(job_id)
                    && output
                        .get("provenance")
                        .and_then(|value| value.get("request_sha256"))
                        .and_then(Value::as_str)
                        == Some(request_sha256)
            })
            .collect::<Vec<_>>();
        let [output] = matching.as_slice() else {
            return Ok(None);
        };
        let provenance = output.get("provenance").ok_or_else(|| {
            VideoServiceError::new(
                "video.store_contract_failed",
                "A recovered render output has no provenance",
            )
        })?;
        if provenance.get("manifest_revision").and_then(Value::as_i64) != Some(expectation.revision)
            || provenance.get("source_version_id").and_then(Value::as_str)
                != Some(expectation.version_id.as_str())
            || provenance.get("variation").and_then(Value::as_u64) != Some(u64::from(variation))
            || output.get("kind").and_then(Value::as_str) != Some(expected_kind)
            || output.get("is_primary").and_then(Value::as_bool) != Some(expected_primary)
        {
            return Ok(None);
        }
        let render_key = provenance
            .get(render_key_field)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_store_shape("outputs.provenance.render_cache_key"))?;
        let expected_id = stable_output_id(
            project
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_store_shape("id"))?,
            current.revision,
            semantic_role,
            render_key,
            variation,
        );
        if output.get("id").and_then(Value::as_str) != Some(expected_id.as_str()) {
            return Ok(None);
        }
        let expected_sha256 = value_string(output, "sha256")?;
        if !manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.cache_key == render_key && artifact.sha256 == expected_sha256)
        {
            return Ok(None);
        }
        if let Some(caption_key_field) = caption_key_field {
            let caption_key = provenance
                .get(caption_key_field)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_store_shape("outputs.provenance.caption_cache_key"))?;
            if !manifest
                .render_artifacts
                .iter()
                .any(|artifact| artifact.cache_key == caption_key)
            {
                return Ok(None);
            }
        }
        self.ensure_not_cancelled(cancel)?;
        let output_path = self.resolve_absolute_managed_path(&PathBuf::from(value_string(
            output,
            "artifact_path",
        )?))?;
        if sha256_file_with_cancel(&output_path, Some(cancel))? != expected_sha256 {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "The atomically published render no longer matches its saved checksum",
            ));
        }
        let runtime = self.runtime_status(false);
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to recover a completed render",
        )?;
        let probe = probe_media(&output_path, ffprobe)?;
        if probe.primary_video_stream.is_none() || probe.duration_us <= 0 {
            return Err(VideoServiceError::new(
                "video.invalid_render",
                "The atomically published render is no longer playable",
            ));
        }
        Ok(Some((*output).clone()))
    }

    #[allow(clippy::too_many_arguments)]
    fn checkpoint_stage(
        &self,
        project_id: &str,
        version_id: Option<&str>,
        stage_key: &str,
        scope_key: &str,
        job_id: &str,
        status: &str,
        resource_class: &str,
        progress: f64,
        input_sha256: &str,
        output_sha256: Option<&str>,
        checkpoint: Value,
        error: Option<Value>,
    ) -> ServiceResult<Value> {
        let resolved_version_id = match version_id {
            Some(version_id) if !version_id.trim().is_empty() => version_id.to_string(),
            _ => project_expectation(&self.get_project(project_id)?)?.version_id,
        };
        self.store
            .upsert_video_stage(&json!({
                "project_id": project_id,
                "version_id": resolved_version_id,
                "stage_key": stage_key,
                "scope_key": scope_key,
                "job_id": job_id,
                "status": status,
                "resource_class": resource_class,
                "attempt": 1,
                "progress": progress.clamp(0.0, 1.0),
                "input_sha256": input_sha256,
                "output_sha256": output_sha256,
                "checkpoint": checkpoint,
                "error": error,
            }))
            .map_err(VideoServiceError::store)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_render_plan(
        &self,
        job_id: &str,
        project_id: &str,
        plan: &RenderCommandPlan,
        expected_duration_us: Option<i64>,
        progress_start: f64,
        progress_end: f64,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        let timeout = render_timeout(expected_duration_us, plan.profile)?;
        let mut staging_cleanup = StagedRenderCleanup::for_plan(plan);
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(|| Instant::now() + MAX_RENDER_DURATION);
        let first = self.execute_render_command(
            job_id,
            project_id,
            &plan.primary,
            expected_duration_us,
            deadline,
            timeout,
            progress_start,
            progress_end,
            cancel,
            callback,
        );
        let result = match first {
            Ok(()) => Ok(()),
            Err(RenderRunError::Cancelled(error)) => Err(error),
            Err(RenderRunError::Failed { error, stderr }) => {
                if error.code == "video.render_timeout" {
                    return Err(error);
                }
                let Some(fallback) = plan.command_after_failure(&stderr) else {
                    return Err(error);
                };
                if fallback.output.is_file() {
                    fs::remove_file(&fallback.output).map_err(|remove_error| {
                        VideoServiceError::io(
                            "video.render_fallback_failed",
                            "The failed hardware render could not be cleared",
                            remove_error,
                        )
                    })?;
                }
                emit_progress(
                    callback,
                    job_id,
                    project_id,
                    "software_fallback",
                    progress_start,
                    "Hardware encoding was unavailable; continuing safely with software encoding",
                    None,
                    None,
                );
                match self.execute_render_command(
                    job_id,
                    project_id,
                    fallback,
                    expected_duration_us,
                    deadline,
                    timeout,
                    progress_start,
                    progress_end,
                    cancel,
                    callback,
                ) {
                    Ok(()) => Ok(()),
                    Err(RenderRunError::Cancelled(error)) => Err(error),
                    Err(RenderRunError::Failed { error, .. }) => Err(error),
                }
            }
        };
        if result.is_ok() {
            staging_cleanup.disarm();
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_render_progress_chunk(
        &self,
        job_id: &str,
        project_id: &str,
        chunk: &[u8],
        expected_duration_us: Option<i64>,
        progress_start: f64,
        progress_end: f64,
        callback: Option<&ProgressCallback>,
        parser: &mut FfmpegProgressParser,
        diagnostic: &mut Vec<u8>,
        last_persisted: &mut f64,
    ) {
        extend_bounded(diagnostic, chunk, MAX_DIAGNOSTIC_BYTES);
        for record in parser.push(chunk, expected_duration_us) {
            if let Some(fraction) = record.fraction {
                let progress =
                    progress_start + fraction.clamp(0.0, 1.0) * (progress_end - progress_start);
                if progress - *last_persisted >= 0.01 || fraction >= 1.0 {
                    let _ = self.store.update_job(job_id, "running", progress);
                    *last_persisted = progress;
                }
                emit_progress(
                    callback,
                    job_id,
                    project_id,
                    "rendering",
                    progress,
                    "Rendering local video",
                    None,
                    Some(json!({
                        "fps": record.fps,
                        "speed": record.speed,
                        "out_time_us": record.out_time_us,
                    })),
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_render_command(
        &self,
        job_id: &str,
        project_id: &str,
        render: &RenderCommand,
        expected_duration_us: Option<i64>,
        deadline: Instant,
        timeout: Duration,
        progress_start: f64,
        progress_end: f64,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> Result<(), RenderRunError> {
        let mut command = render.command();
        // FFmpeg writes staged media directly. Apply an owner-only child umask
        // before exec so even in-progress files never acquire ambient 0644.
        unsafe {
            command.pre_exec(|| {
                libc::umask(0o077);
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|error| RenderRunError::Failed {
            error: VideoServiceError::io(
                "video.render_start_failed",
                "FFmpeg could not be started",
                error,
            ),
            stderr: String::new(),
        })?;
        let child_id = child.id();
        let mut stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                return Err(RenderRunError::Failed {
                    error: VideoServiceError::new(
                        "video.render_start_failed",
                        "FFmpeg did not expose its progress stream",
                    ),
                    stderr: String::new(),
                });
            }
        };
        if let Err(error) = set_capture_pipe_nonblocking(&stderr) {
            let _ = terminate_process_group(&mut child, Duration::from_secs(2));
            return Err(RenderRunError::Failed {
                error: VideoServiceError::io(
                    "video.render_progress_failed",
                    "FFmpeg progress could not be secured",
                    error,
                ),
                stderr: String::new(),
            });
        }
        let mut parser = FfmpegProgressParser::default();
        let mut diagnostic = Vec::new();
        let mut stderr_eof = false;
        let mut last_persisted = progress_start - 0.02;
        let status = loop {
            if cancel.load(Ordering::Acquire) {
                let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                return Err(RenderRunError::Cancelled(VideoServiceError::cancelled()));
            }
            if Instant::now() >= deadline {
                let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                return Err(RenderRunError::Failed {
                    error: VideoServiceError::new(
                        "video.render_timeout",
                        "FFmpeg exceeded the bounded local render deadline",
                    )
                    .retryable(true)
                    .details(json!({
                        "timeout_seconds": timeout.as_secs(),
                        "expected_duration_us": expected_duration_us,
                        "encoder": render.encoder,
                    })),
                    stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
                });
            }
            if !stderr_eof {
                match read_nonblocking_chunks(&mut stderr, 32) {
                    Ok((chunks, reached_eof)) => {
                        stderr_eof = reached_eof;
                        for chunk in chunks {
                            self.consume_render_progress_chunk(
                                job_id,
                                project_id,
                                &chunk,
                                expected_duration_us,
                                progress_start,
                                progress_end,
                                callback,
                                &mut parser,
                                &mut diagnostic,
                                &mut last_persisted,
                            );
                        }
                    }
                    Err(error) => {
                        let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                        return Err(RenderRunError::Failed {
                            error: VideoServiceError::io(
                                "video.render_progress_failed",
                                "FFmpeg progress could not be read",
                                error,
                            ),
                            stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
                        });
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                    return Err(RenderRunError::Failed {
                        error: VideoServiceError::io(
                            "video.render_monitor_failed",
                            "FFmpeg could not be monitored",
                            error,
                        ),
                        stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
                    });
                }
            }
            thread::sleep(COMMAND_POLL_INTERVAL.min(Duration::from_millis(20)));
        };

        // The FFmpeg leader may exit while a helper still owns its stderr.
        // Drain without blocking for a fixed interval, then kill the dedicated
        // process group and fail closed instead of joining an unbounded reader.
        let drain_deadline = Instant::now()
            .checked_add(COMMAND_PIPE_DRAIN_TIMEOUT)
            .unwrap_or_else(Instant::now);
        while !stderr_eof && Instant::now() < drain_deadline {
            match read_nonblocking_chunks(&mut stderr, 32) {
                Ok((chunks, reached_eof)) => {
                    stderr_eof = reached_eof;
                    let had_output = !chunks.is_empty();
                    for chunk in chunks {
                        self.consume_render_progress_chunk(
                            job_id,
                            project_id,
                            &chunk,
                            expected_duration_us,
                            progress_start,
                            progress_end,
                            callback,
                            &mut parser,
                            &mut diagnostic,
                            &mut last_persisted,
                        );
                    }
                    if !had_output && !stderr_eof {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                Err(error) => {
                    kill_process_group_by_id(child_id);
                    return Err(RenderRunError::Failed {
                        error: VideoServiceError::io(
                            "video.render_progress_failed",
                            "FFmpeg progress could not be drained",
                            error,
                        ),
                        stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
                    });
                }
            }
        }
        if !stderr_eof {
            kill_process_group_by_id(child_id);
            return Err(RenderRunError::Failed {
                error: VideoServiceError::new(
                    "video.render_pipe_open",
                    "FFmpeg left its progress pipe open after exiting",
                )
                .retryable(true),
                stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
            });
        }
        if status.success() {
            secure_managed_file(&render.output).map_err(|error| RenderRunError::Failed {
                error,
                stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
            })
        } else {
            let stderr = String::from_utf8_lossy(&diagnostic).into_owned();
            Err(RenderRunError::Failed {
                error: VideoServiceError::new(
                    "video.render_failed",
                    "FFmpeg could not render the requested video",
                )
                .details(json!({
                    "encoder": render.encoder,
                    "diagnostic": truncate_chars(&stderr, 4_000),
                })),
                stderr,
            })
        }
    }

    fn project_dir(&self, project_id: &str) -> ServiceResult<PathBuf> {
        validate_safe_component(project_id, "video.invalid_project_id")?;
        let directory = self.video_root.join("projects").join(project_id);
        secure_managed_directory_path(&self.video_root, &directory)?;
        Ok(directory)
    }

    fn secure_managed_directory(&self, directory: &Path) -> ServiceResult<()> {
        secure_managed_directory_path(&self.video_root, directory)
    }

    fn secure_managed_flat_directory(&self, directory: &Path) -> ServiceResult<()> {
        self.secure_managed_directory(directory)?;
        for entry in fs::read_dir(directory).map_err(|error| {
            VideoServiceError::io(
                "video.storage_unavailable",
                "A managed directory could not be inspected",
                error,
            )
        })? {
            let entry = entry.map_err(|error| {
                VideoServiceError::io(
                    "video.storage_unavailable",
                    "A managed directory entry could not be inspected",
                    error,
                )
            })?;
            secure_managed_file(&entry.path())?;
        }
        Ok(())
    }

    fn cache_path(
        &self,
        namespace: &str,
        cache_key: &str,
        extension: &str,
    ) -> ServiceResult<PathBuf> {
        validate_safe_component(namespace, "video.invalid_cache_namespace")?;
        validate_hash(cache_key)?;
        validate_safe_component(extension, "video.invalid_extension")?;
        let directory = self.video_root.join("cache").join(namespace);
        secure_managed_directory_path(&self.video_root, &directory)?;
        Ok(directory.join(format!("{cache_key}.{extension}")))
    }

    fn relative_managed_path(&self, path: &Path) -> ServiceResult<String> {
        let root = fs::canonicalize(&self.video_root).map_err(|error| {
            VideoServiceError::io(
                "video.storage_unavailable",
                "Managed video storage could not be resolved",
                error,
            )
        })?;
        let path = fs::canonicalize(path).map_err(|error| {
            VideoServiceError::io(
                "video.artifact_not_found",
                "The managed artifact could not be resolved",
                error,
            )
        })?;
        let relative = path.strip_prefix(&root).map_err(|_| {
            VideoServiceError::new(
                "video.unsafe_artifact_path",
                "The artifact is outside managed video storage",
            )
        })?;
        secure_managed_file(&path)?;
        relative.to_str().map(str::to_string).ok_or_else(|| {
            VideoServiceError::new(
                "video.invalid_artifact_path",
                "Managed artifact paths must be valid UTF-8",
            )
        })
    }

    fn resolve_managed_path(&self, relative: &str) -> ServiceResult<PathBuf> {
        let path = Path::new(relative);
        if relative.is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(VideoServiceError::new(
                "video.unsafe_artifact_path",
                "The manifest contains an unsafe managed path",
            ));
        }
        self.resolve_absolute_managed_path(&self.video_root.join(path))
    }

    fn resolve_absolute_managed_path(&self, path: &Path) -> ServiceResult<PathBuf> {
        let root = fs::canonicalize(&self.video_root).map_err(|error| {
            VideoServiceError::io(
                "video.storage_unavailable",
                "Managed video storage could not be resolved",
                error,
            )
        })?;
        let path = fs::canonicalize(path).map_err(|error| {
            VideoServiceError::io(
                "video.artifact_not_found",
                "The managed artifact could not be opened",
                error,
            )
        })?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(VideoServiceError::new(
                "video.unsafe_artifact_path",
                "The artifact is outside managed video storage",
            ));
        }
        secure_managed_file(&path)?;
        Ok(path)
    }

    fn cached_product(
        &self,
        _project_id: &str,
        cache_key: &str,
        kind: &str,
        mime_type: &str,
        role: RenderArtifactRole,
        ffprobe: Option<&Path>,
    ) -> ServiceResult<Option<DerivedProduct>> {
        let Some(cache) = self
            .store
            .get_video_cache(cache_key)
            .map_err(VideoServiceError::store)?
        else {
            return Ok(None);
        };
        let path = self.resolve_absolute_managed_path(&PathBuf::from(value_string(
            &cache,
            "artifact_path",
        )?))?;
        let product = if let Some(ffprobe) = ffprobe {
            product_from_media(
                &self.video_root,
                path.clone(),
                cache_key.to_string(),
                kind,
                mime_type,
                role,
                probe_media(&path, ffprobe)?,
            )?
        } else {
            validate_image_file(&path)?;
            let dimensions = match kind {
                "thumbnail" => Some((640, 360)),
                "waveform" => Some((1600, 320)),
                _ => None,
            };
            product_from_image(
                &self.video_root,
                path,
                cache_key.to_string(),
                kind,
                mime_type,
                role,
                dimensions,
            )?
        };
        Ok(Some(product))
    }

    fn typed_render_artifact(
        &self,
        product: &DerivedProduct,
        created_at: &str,
    ) -> ServiceResult<RenderArtifact> {
        let artifact = RenderArtifact {
            id: product.id.clone(),
            role: product.role,
            scene_id: None,
            managed_path: self.relative_managed_path(&product.path)?,
            sha256: product.sha256.clone(),
            cache_key: product.cache_key.clone(),
            mime_type: product.mime_type.clone(),
            duration_us: product.duration_us.map(Microseconds),
            width: product.width,
            height: product.height,
            publication_state: PublicationState::Published,
            created_at: created_at.to_string(),
        };
        artifact.validate()?;
        Ok(artifact)
    }

    fn record_cache_hit(&self, project_id: &str, job_id: &str, operation: &str) {
        let _ = self.store.record_video_performance(&json!({
            "project_id": project_id,
            "job_id": job_id,
            "operation": operation,
            "profile": "cache",
            "wall_seconds": 0.0,
            "cache_hit": true,
            "details": {},
        }));
    }
}

impl VideoStudioService {
    pub fn new(store: Arc<Store>) -> ServiceResult<Self> {
        Self::new_with_optional_gpu_admission_gate(store, None)
    }

    pub fn new_with_gpu_admission_gate(
        store: Arc<Store>,
        gpu_admission_gate: Arc<dyn SharedGpuAdmissionGate>,
    ) -> ServiceResult<Self> {
        Self::new_with_optional_gpu_admission_gate(store, Some(gpu_admission_gate))
    }

    fn new_with_optional_gpu_admission_gate(
        store: Arc<Store>,
        gpu_admission_gate: Option<Arc<dyn SharedGpuAdmissionGate>>,
    ) -> ServiceResult<Self> {
        let video_root = store.video_artifacts_root();
        fs::create_dir_all(&video_root).map_err(|error| {
            VideoServiceError::io(
                "video.storage_unavailable",
                "Video artifact storage could not be created",
                error,
            )
        })?;
        secure_private_directory(&video_root)?;
        Ok(Self {
            store,
            video_root,
            scheduler: Mutex::new(ResourceScheduler::for_rtx_4080_laptop()),
            storage_reservations: Mutex::new(StorageReservationState::default()),
            gpu_admission_gate,
            cancellations: Mutex::new(HashMap::new()),
            runtime_cache: Mutex::new(None),
            #[cfg(test)]
            package_test_barrier: Mutex::new(None),
            #[cfg(test)]
            local_import_test_barrier: Mutex::new(None),
            #[cfg(test)]
            single_render_test_failpoint: Mutex::new(None),
        })
    }

    pub fn runtime_status(&self, refresh: bool) -> MediaRuntimeStatus {
        let mut cache = self
            .runtime_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !refresh {
            if let Some((observed_at, status)) = cache.as_ref() {
                if observed_at.elapsed() < Duration::from_secs(30) {
                    return status.clone();
                }
            }
        }
        let status = discover_media_runtime();
        *cache = Some((Instant::now(), status.clone()));
        status
    }

    pub fn create_project(&self, request: CreateVideoProjectRequest) -> ServiceResult<Value> {
        let mut manifest = request.manifest;
        if manifest.project_id.trim().is_empty()
            || manifest.project_id != manifest.project_id.trim()
        {
            return Err(VideoServiceError::new(
                "video.invalid_project_id",
                "The manifest must contain a stable project identifier",
            ));
        }
        if manifest.name != request.name.trim() {
            manifest.name = request.name.trim().to_string();
        }
        if manifest.revision != 0 || !manifest.revision_history.is_empty() {
            return Err(VideoServiceError::new(
                "video.invalid_initial_revision",
                "A new project manifest must begin at revision zero",
            ));
        }
        let actor = require_text(
            &request.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        let reason = request
            .initial_intent
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("Initial intent: {}", truncate_chars(value, 3_800)))
            .unwrap_or_else(|| "Video project created".to_string());
        manifest.revision = 1;
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: 1,
            parent_id: None,
            actor: actor.to_string(),
            reason,
            changed_paths: vec!["/".to_string()],
            invalidated_stages: BTreeSet::new(),
            created_at: utc_now(),
        });
        manifest.updated_at = utc_now();
        manifest.validate_strict()?;
        let value = serde_json::to_value(&manifest).map_err(json_error)?;
        self.store
            .create_video_project(&manifest.name, &value, actor)
            .map_err(VideoServiceError::store)
    }

    pub fn list_projects(&self) -> ServiceResult<Vec<Value>> {
        self.store
            .list_video_projects()
            .map_err(VideoServiceError::store)
    }

    pub fn get_project(&self, project_id: &str) -> ServiceResult<Value> {
        self.store
            .get_video_project(project_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new("video.not_found", "The video project was not found")
            })
    }

    pub fn revise_manifest(&self, request: ReviseVideoManifestRequest) -> ServiceResult<Value> {
        let actor = require_text(
            &request.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        let reason = require_text(
            &request.reason,
            "video.invalid_revision",
            "A revision reason is required",
        )?;
        if request.manifest.project_id != request.project_id {
            return Err(VideoServiceError::new(
                "video.invalid_manifest",
                "The manifest project identifier does not match the target project",
            ));
        }
        let current = self.get_project(&request.project_id)?;
        let current_db_revision = value_i64(&current, "revision")?;
        if current_db_revision != request.expected_revision {
            return Err(VideoServiceError::new(
                "video.revision_conflict",
                format!(
                    "Expected revision {}, but the project is at revision {}",
                    request.expected_revision, current_db_revision
                ),
            )
            .retryable(true));
        }
        let current_manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        current_manifest.validate_strict()?;
        if i64::try_from(current_manifest.revision).ok() != Some(current_db_revision) {
            return Err(VideoServiceError::new(
                "video.revision_integrity_failed",
                "The stored manifest and project revision are not aligned",
            ));
        }
        if request.manifest.created_at != current_manifest.created_at {
            return Err(VideoServiceError::new(
                "video.immutable_manifest_field",
                "The project creation timestamp is immutable",
            ));
        }
        if request.manifest.revision != current_manifest.revision + 1 {
            return Err(VideoServiceError::new(
                "video.invalid_revision",
                "The replacement manifest must advance exactly one revision",
            ));
        }
        let expected_parent = current_manifest
            .revision_history
            .last()
            .map(|record| record.id.as_str());
        let record = request.manifest.revision_history.last().ok_or_else(|| {
            VideoServiceError::new(
                "video.invalid_revision",
                "The replacement manifest is missing its revision record",
            )
        })?;
        if request.manifest.revision_history.len() != current_manifest.revision_history.len() + 1
            || request.manifest.revision_history[..current_manifest.revision_history.len()]
                != current_manifest.revision_history[..]
        {
            return Err(VideoServiceError::new(
                "video.revision_history_modified",
                "Existing revision history is immutable and must remain an exact prefix",
            ));
        }
        if record.parent_id.as_deref() != expected_parent
            || record.actor != actor
            || record.reason != reason
        {
            return Err(VideoServiceError::new(
                "video.invalid_revision",
                "The replacement manifest revision metadata does not match the request",
            ));
        }
        let actual_paths = manifest_changed_paths(&current_manifest, &request.manifest)?;
        if actual_paths.is_empty() {
            return Err(VideoServiceError::new(
                "video.empty_revision",
                "The replacement manifest does not change any versioned content",
            ));
        }
        let requested_paths = request
            .changed_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let recorded_paths = record
            .changed_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if requested_paths.len() != request.changed_paths.len()
            || recorded_paths.len() != record.changed_paths.len()
            || requested_paths != actual_paths
            || recorded_paths != actual_paths
        {
            return Err(VideoServiceError::new(
                "video.revision_diff_mismatch",
                "changed_paths must exactly describe the manifest content diff",
            )
            .details(json!({
                "actual_changed_paths": actual_paths,
                "requested_changed_paths": requested_paths,
                "recorded_changed_paths": recorded_paths,
            })));
        }
        let inferred_stages = invalidated_stages_for_manifest_changes(&actual_paths);
        if request.invalidated_stages != inferred_stages
            || record.invalidated_stages != inferred_stages
        {
            return Err(VideoServiceError::new(
                "video.revision_invalidation_mismatch",
                "invalidated_stages must exactly match the manifest diff",
            )
            .details(json!({
                "expected_invalidated_stages": inferred_stages,
                "requested_invalidated_stages": request.invalidated_stages,
                "recorded_invalidated_stages": record.invalidated_stages,
            })));
        }
        request.manifest.validate_strict()?;
        let lock = ProjectLock::acquire(self, &request.project_id, actor)?;
        let value = serde_json::to_value(&request.manifest).map_err(json_error)?;
        let result = self
            .store
            .commit_video_manifest(
                &request.project_id,
                request.expected_revision,
                &value,
                actor,
                reason,
                &lock.token,
                request.status.as_deref(),
            )
            .map_err(VideoServiceError::store);
        drop(lock);
        result
    }

    /// Applies one idempotent batch of source-clock timeline edits and returns the authoritative
    /// project. The durable edit job, revision CAS, project lock, manifest version, and terminal
    /// completion are committed as one observable operation; a crash replay adopts the exact
    /// revision instead of applying the edit twice.
    pub fn edit_timeline(
        &self,
        request: VideoTimelineEditRequest,
    ) -> ServiceResult<TimelineEditServiceResult> {
        let request_value = serde_json::to_value(&request).map_err(json_error)?;
        let idempotency_key = format!(
            "timeline-edit:{}",
            sha256_bytes(format!("{}:{}", request.project_id, request.operation_id).as_bytes())
        );
        let Some((job_id, created)) = self
            .store
            .create_idempotent_job("video_edit_timeline", &idempotency_key, &request_value)
            .map_err(VideoServiceError::store)?
        else {
            return Err(VideoServiceError::new(
                "video.idempotency_conflict",
                "This timeline operation identifier was already used with a different request",
            ));
        };

        let result = self.edit_timeline_with_job(&request, &job_id, created);
        if let Err(error) = &result {
            let _ = self.store.fail_job(&job_id, &error.stable_message());
        }
        result
    }

    fn edit_timeline_with_job(
        &self,
        request: &VideoTimelineEditRequest,
        job_id: &str,
        created: bool,
    ) -> ServiceResult<TimelineEditServiceResult> {
        let reason = format!("Timeline edit {}", request.operation_id);
        let actor = "video-studio-editor";
        let existing_job = self
            .store
            .get_job(job_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.job_not_found",
                    "The durable timeline edit job was not found",
                )
            })?;
        let existing_status = existing_job
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_store_shape("job.status"))?;

        if !created && matches!(existing_status, "preparing" | "running" | "queued") {
            return Err(VideoServiceError::new(
                "video.operation_in_progress",
                "This timeline edit is already running",
            )
            .retryable(true)
            .details(json!({ "job_id": job_id })));
        }

        if !created && matches!(existing_status, "failed" | "cancelled") {
            self.store
                .resume_video_job(job_id, &["video_edit_timeline"])
                .map_err(VideoServiceError::store)?;
        }

        let lock = ProjectLock::acquire(self, &request.project_id, actor)?;
        let current = self.get_project(&request.project_id)?;
        let current_manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        current_manifest.validate_strict()?;

        if let Some(record) = current_manifest.revision_history.iter().find(|record| {
            record.reason == reason
                && record.actor == actor
                && record.revision == request.expected_revision.saturating_add(1)
        }) {
            if existing_status != "completed" {
                let completed = self
                    .store
                    .complete_job(job_id)
                    .map_err(VideoServiceError::store)?;
                if !completed {
                    return Err(VideoServiceError::new(
                        "video.cancelled",
                        "The timeline edit completed before cancellation and is ready to reload",
                    )
                    .details(json!({ "job_id": job_id, "project_id": request.project_id })));
                }
            }
            drop(lock);
            return Ok(TimelineEditServiceResult {
                project: current,
                receipt: VideoTimelineChangeReceipt {
                    project_id: request.project_id.clone(),
                    expected_revision: request.expected_revision,
                    base_version_id: request.base_version_id.clone(),
                    operation_id: request.operation_id.clone(),
                    changed_paths: record.changed_paths.clone(),
                    invalidated_stages: record.invalidated_stages.clone(),
                },
                job_id: job_id.to_string(),
                replayed: true,
            });
        }

        if existing_status == "completed" {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "The completed timeline edit has no matching project revision",
            ));
        }
        self.store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;

        let expectation = ProjectExpectation {
            revision: i64::try_from(request.expected_revision).map_err(|_| {
                VideoServiceError::new(
                    "video.invalid_revision",
                    "The timeline edit revision is outside the supported range",
                )
            })?,
            version_id: request.base_version_id.clone(),
        };
        ensure_project_matches(&current, &expectation)?;
        let applied = apply_timeline_edit(&current_manifest, request)?;
        let mut manifest = applied.manifest;
        let next_revision = manifest.revision.checked_add(1).ok_or_else(|| {
            VideoServiceError::new(
                "video.revision_overflow",
                "The timeline edit revision could not be advanced",
            )
        })?;
        manifest.revision = next_revision;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: next_revision,
            parent_id: manifest
                .revision_history
                .last()
                .map(|record| record.id.clone()),
            actor: actor.to_string(),
            reason: reason.clone(),
            changed_paths: applied.receipt.changed_paths.clone(),
            invalidated_stages: applied.receipt.invalidated_stages.clone(),
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict()?;
        let manifest_value = serde_json::to_value(&manifest).map_err(json_error)?;
        let status = current.get("status").and_then(Value::as_str);
        let project = self
            .store
            .commit_video_manifest_and_complete_job(
                &request.project_id,
                expectation.revision,
                &manifest_value,
                actor,
                &reason,
                &lock.token,
                status,
                job_id,
            )
            .map_err(VideoServiceError::store)?;
        drop(lock);
        Ok(TimelineEditServiceResult {
            project,
            receipt: applied.receipt,
            job_id: job_id.to_string(),
            replayed: false,
        })
    }

    /// Applies one cast and written script and returns the authoritative project.
    ///
    /// The receipt separates turns whose words survived from turns that are genuinely new, so the
    /// caller renders only what changed. Like `edit_timeline`, the durable job, revision CAS,
    /// project lock, and terminal completion commit as one observable operation, and a crash
    /// replay adopts the existing revision instead of applying the script twice.
    pub fn apply_script(&self, request: VideoScriptRequest) -> ServiceResult<ScriptServiceResult> {
        let request_value = serde_json::to_value(&request).map_err(json_error)?;
        let idempotency_key = format!(
            "apply-script:{}",
            sha256_bytes(format!("{}:{}", request.project_id, request.operation_id).as_bytes())
        );
        let Some((job_id, created)) = self
            .store
            .create_idempotent_job("video_apply_script", &idempotency_key, &request_value)
            .map_err(VideoServiceError::store)?
        else {
            return Err(VideoServiceError::new(
                "video.idempotency_conflict",
                "This script operation identifier was already used with a different request",
            ));
        };

        let result = self.apply_script_with_job(&request, &job_id, created);
        if let Err(error) = &result {
            let _ = self.store.fail_job(&job_id, &error.stable_message());
        }
        result
    }

    fn apply_script_with_job(
        &self,
        request: &VideoScriptRequest,
        job_id: &str,
        created: bool,
    ) -> ServiceResult<ScriptServiceResult> {
        let reason = format!("Cast and script {}", request.operation_id);
        let actor = "video-studio-writer";
        let existing_job = self
            .store
            .get_job(job_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.job_not_found",
                    "The durable script job was not found",
                )
            })?;
        let existing_status = existing_job
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_store_shape("job.status"))?;

        if !created && matches!(existing_status, "preparing" | "running" | "queued") {
            return Err(VideoServiceError::new(
                "video.operation_in_progress",
                "This script is already being applied",
            )
            .retryable(true)
            .details(json!({ "job_id": job_id })));
        }
        if !created && matches!(existing_status, "failed" | "cancelled") {
            self.store
                .resume_video_job(job_id, &["video_apply_script"])
                .map_err(VideoServiceError::store)?;
        }

        let lock = ProjectLock::acquire(self, &request.project_id, actor)?;
        let current = self.get_project(&request.project_id)?;
        let current_manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        current_manifest.validate_strict()?;

        // A replay after a crash between commit and job completion finds its own revision
        // already recorded and adopts it rather than writing the script a second time.
        if let Some(record) = current_manifest.revision_history.iter().find(|record| {
            record.reason == reason
                && record.actor == actor
                && record.revision == request.expected_revision.saturating_add(1)
        }) {
            if existing_status != "completed" {
                let completed = self
                    .store
                    .complete_job(job_id)
                    .map_err(VideoServiceError::store)?;
                if !completed {
                    return Err(VideoServiceError::new(
                        "video.cancelled",
                        "The script was applied before cancellation and is ready to reload",
                    )
                    .details(json!({ "job_id": job_id, "project_id": request.project_id })));
                }
            }
            let turn_ids = current_manifest
                .dialogue
                .iter()
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>();
            drop(lock);
            return Ok(ScriptServiceResult {
                project: current,
                receipt: VideoScriptReceipt {
                    project_id: request.project_id.clone(),
                    expected_revision: request.expected_revision,
                    base_version_id: request.base_version_id.clone(),
                    operation_id: request.operation_id.clone(),
                    changed_paths: record.changed_paths.clone(),
                    invalidated_stages: record.invalidated_stages.clone(),
                    // A replay cannot reconstruct which turns were new at the time, and guessing
                    // would invite a duplicate render. Reporting every turn as retained keeps the
                    // caller from re-reading work the committed revision already owns.
                    retained_turn_ids: turn_ids,
                    new_turn_ids: Vec::new(),
                    dropped_binding_ids: Vec::new(),
                },
                job_id: job_id.to_string(),
                replayed: true,
            });
        }

        if existing_status == "completed" {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "The completed script job has no matching project revision",
            ));
        }
        self.store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;

        let expectation = ProjectExpectation {
            revision: i64::try_from(request.expected_revision).map_err(|_| {
                VideoServiceError::new(
                    "video.invalid_revision",
                    "The script revision is outside the supported range",
                )
            })?,
            version_id: request.base_version_id.clone(),
        };
        ensure_project_matches(&current, &expectation)?;
        let applied = apply_dialogue_script(
            &current_manifest,
            &DialogueScriptRequest {
                cast: request.cast.clone(),
                script: request.script.clone(),
            },
        )?;
        let mut manifest = applied.manifest;
        let next_revision = manifest.revision.checked_add(1).ok_or_else(|| {
            VideoServiceError::new(
                "video.revision_overflow",
                "The script revision could not be advanced",
            )
        })?;
        manifest.revision = next_revision;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: next_revision,
            parent_id: manifest
                .revision_history
                .last()
                .map(|record| record.id.clone()),
            actor: actor.to_string(),
            reason: reason.clone(),
            changed_paths: applied.changed_paths.clone(),
            invalidated_stages: applied.invalidated_stages.clone(),
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict()?;
        let manifest_value = serde_json::to_value(&manifest).map_err(json_error)?;
        let status = current.get("status").and_then(Value::as_str);
        let project = self
            .store
            .commit_video_manifest_and_complete_job(
                &request.project_id,
                expectation.revision,
                &manifest_value,
                actor,
                &reason,
                &lock.token,
                status,
                job_id,
            )
            .map_err(VideoServiceError::store)?;
        drop(lock);
        Ok(ScriptServiceResult {
            project,
            receipt: VideoScriptReceipt {
                project_id: request.project_id.clone(),
                expected_revision: request.expected_revision,
                base_version_id: request.base_version_id.clone(),
                operation_id: request.operation_id.clone(),
                changed_paths: applied.changed_paths,
                invalidated_stages: applied.invalidated_stages,
                retained_turn_ids: applied.retained_turn_ids,
                new_turn_ids: applied.new_turn_ids,
                dropped_binding_ids: applied.dropped_binding_ids,
            },
            job_id: job_id.to_string(),
            replayed: false,
        })
    }

    /// Describe the episode as rendered, so a revision responds to something measured.
    ///
    /// Strictly read-only. Every number comes from a published artifact with a measured duration;
    /// a value that was never measured is absent rather than approximated, because an
    /// approximation the assistant cannot distinguish from a measurement is worse than no value.
    pub fn listen_to_episode(
        &self,
        project_id: &str,
        loudness: Option<LoudnessMeasurement>,
    ) -> ServiceResult<EpisodeListening> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        Ok(listen_to_episode(&manifest, loudness)?)
    }

    /// Give a dialogue-only episode the scene it needs to be rendered.
    ///
    /// A scene is the author's division of an episode, and a script written as dialogue has none
    /// yet. Rendering, captions, and chapters are all scene-shaped, so an episode performed from a
    /// script would otherwise be unrenderable. One scene spanning the performed dialogue is the
    /// honest default: it is the whole episode until the author divides it.
    ///
    /// Does nothing when the project already has scenes, so an author's own divisions are never
    /// replaced by a generated one.
    /// The track generated clips are laid on. Named so it can be recognised and replaced whole.
    const CLIP_TRACK_ID: &'static str = "generated-clips";

    /// Whether this machine can generate moving clips at all.
    pub fn clip_generation_available(&self, models: Option<&Path>) -> bool {
        let runtime = self.runtime_status(false);
        runtime.sd_cli.available
            && models.is_some_and(|dir| super::media::resolve_clip_models(dir).is_ok())
    }

    /// Generate this episode's shots and cut them across its narration.
    ///
    /// A clip costs about a minute of compute for under two seconds of footage, so an episode is
    /// never covered one-to-one. It is covered by a handful of distinct shots, repeated across the
    /// clock, which is how b-roll has always worked. Each shot is content-addressed by its prompt,
    /// so re-running this on an unchanged episode regenerates nothing.
    ///
    /// The shots are described by the caller. An episode's own words describe a conversation, and
    /// a shot has to say what is on screen - deriving one from the other produces a sentence about
    /// a podcast, which a video model renders as nothing recognisable.
    pub fn generate_episode_clips(
        &self,
        project_id: &str,
        actor: &str,
        model_directory: &Path,
        descriptions: &[String],
        cancel: &AtomicBool,
    ) -> ServiceResult<Value> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;

        let span_us = self.episode_span_us(&manifest);
        if span_us <= 0 {
            return Err(VideoServiceError::new(
                "video.clip_span_unknown",
                "This episode has no duration yet, so there is nothing for clips to cover",
            ));
        }

        let runtime = self.runtime_status(false);
        let sd_cli = required_tool_path(
            &runtime.sd_cli,
            "video.sd_cli_unavailable",
            "The local video generator is required to generate clips",
        )?;
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to assemble generated clips",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to measure generated clips",
        )?;
        let models = super::media::resolve_clip_models(model_directory)
            .map_err(|error| VideoServiceError::new("video.clip_model_missing", error.message))?;

        let (width, height) = CLIP_CANVAS;
        let shots = super::shots::plan_shots(descriptions, span_us, CLIP_STYLE);
        if shots.is_empty() {
            return Err(VideoServiceError::new(
                "video.shots_not_described",
                "Describe what each shot shows before generating clips",
            ));
        }

        let mut artifacts = Vec::new();
        for shot in &shots {
            self.ensure_not_cancelled(cancel)?;
            let cache_key = shot.cache_key();
            let output = self.cache_path("clip", &cache_key, "mp4")?;
            if !output.is_file() {
                self.render_one_clip(
                    sd_cli, ffmpeg, &models, shot, width, height, &cache_key, &output,
                )?;
            }
            let probe = probe_media(&output, ffprobe)?;
            let artifact = RenderArtifact {
                id: format!("clip-{}", &cache_key[..24]),
                role: RenderArtifactRole::GeneratedClip,
                scene_id: None,
                managed_path: self.relative_managed_path(&output)?,
                sha256: sha256_file(&output)?,
                cache_key,
                mime_type: "video/mp4".to_string(),
                duration_us: Some(Microseconds(probe.duration_us)),
                width: Some(width),
                height: Some(height),
                publication_state: PublicationState::Published,
                created_at: utc_now(),
            };
            artifact.validate()?;
            artifacts.push(artifact);
        }

        // A small number of clips is cut across the whole episode rather than one clip per second.
        let placements = super::shots::tile_shots(artifacts.len(), span_us);
        let clips = placements
            .iter()
            .enumerate()
            .map(|(position, (shot_index, start, duration))| {
                let artifact = &artifacts[*shot_index];
                Ok(TimelineClip {
                    id: format!("clip-cut-{position:04}"),
                    scene_id: None,
                    turn_id: None,
                    media: super::MediaReference {
                        source_asset_id: None,
                        render_artifact_id: Some(artifact.id.clone()),
                    },
                    source_range: TimeRange::new(0, duration.0)?,
                    timeline_start_us: *start,
                    timeline_duration_us: *duration,
                    playback_rate: RationalRate::ONE,
                    gain_db_milli: 0,
                    // A generated clip supplies picture only; the episode's own narration is the
                    // sound, and a clip's incidental audio must never talk over it.
                    muted: true,
                    crop: None,
                })
            })
            .collect::<Result<Vec<_>, VideoError>>()?;

        let expectation = project_expectation(&record)?;
        let shot_count = artifacts.len();
        self.commit_manifest_mutation_at_if_parent_active(
            project_id,
            &expectation,
            actor,
            &format!("Generate {shot_count} shot(s) and cut them across the episode"),
            None,
            vec![
                "/render_artifacts".to_string(),
                "/tracks".to_string(),
                "/visual_assets".to_string(),
                "/visual_layers".to_string(),
            ],
            BTreeSet::from([
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            None,
            move |manifest: &mut VideoProjectManifest| {
                // Replace the previous set outright: a regenerated episode gets one clip track,
                // never last run's cuts with this run's stacked on top.
                manifest
                    .render_artifacts
                    .retain(|existing| !matches!(existing.role, RenderArtifactRole::GeneratedClip));
                manifest
                    .tracks
                    .retain(|track| track.id != Self::CLIP_TRACK_ID);
                // Moving shots replace a drawn card. The card is a full-canvas layer, so leaving
                // it in place would paint it straight over the clips it was standing in for.
                let cards = manifest
                    .visual_assets
                    .iter()
                    .filter(|asset| asset.id.starts_with("cover-"))
                    .map(|asset| asset.id.clone())
                    .collect::<Vec<_>>();
                manifest
                    .visual_layers
                    .retain(|layer| !cards.contains(&layer.asset_id));
                manifest
                    .visual_assets
                    .retain(|asset| !asset.id.starts_with("cover-"));
                manifest.render_artifacts.extend(artifacts.clone());
                manifest.tracks.push(TimelineTrack {
                    id: Self::CLIP_TRACK_ID.to_string(),
                    kind: TrackKind::Video,
                    preserve_gaps: false,
                    clips: clips.clone(),
                });
                Ok(())
            },
        )
    }

    /// Generate one shot and encode it into a managed clip.
    #[allow(clippy::too_many_arguments)]
    fn render_one_clip(
        &self,
        sd_cli: &Path,
        ffmpeg: &Path,
        models: &ClipModelPaths,
        shot: &super::shots::ShotPlan,
        width: u32,
        height: u32,
        cache_key: &str,
        output: &Path,
    ) -> ServiceResult<()> {
        let frames_dir = self.video_root.join("cache").join("clip").join(cache_key);
        self.secure_managed_directory(&frames_dir)?;
        let pattern = frames_dir.join("f_%03d.png");
        // The seed is derived from the prompt, so the same shot is the same footage every time.
        let seed = i64::from(u32::from_str_radix(&cache_key[..8], 16).unwrap_or(1));

        let command =
            super::build_clip_command(sd_cli, models, &shot.prompt, width, height, seed, &pattern)
                .map_err(|error| VideoServiceError::new("video.clip_failed", error.message))?;
        self.run_generator(&command, CLIP_GENERATION_TIMEOUT)?;

        // Frames are assembled here rather than by the generator, so nothing is encoded twice.
        let staging = sibling_staging_path(output)?;
        let encode = RenderCommand {
            program: fs::canonicalize(ffmpeg).map_err(|error| {
                VideoServiceError::io(
                    "video.ffmpeg_unavailable",
                    "FFmpeg could not be resolved",
                    error,
                )
            })?,
            args: vec![
                OsString::from("-hide_banner"),
                OsString::from("-loglevel"),
                OsString::from("error"),
                OsString::from("-nostdin"),
                OsString::from("-y"),
                OsString::from("-framerate"),
                OsString::from(CLIP_FPS.to_string()),
                OsString::from("-i"),
                pattern.as_os_str().to_os_string(),
                OsString::from("-c:v"),
                OsString::from("libx264"),
                OsString::from("-preset"),
                OsString::from("medium"),
                OsString::from("-crf"),
                OsString::from("18"),
                OsString::from("-pix_fmt"),
                OsString::from("yuv420p"),
                OsString::from("-movflags"),
                OsString::from("+faststart"),
                // Name the container as well as the codec. The clip is written to a staging path
                // with no extension, and an inferred format is how a generated still ended up a
                // JPEG wearing a .png name once already.
                OsString::from("-f"),
                OsString::from("mp4"),
                staging.as_os_str().to_os_string(),
            ],
            output: staging.clone(),
            encoder: VideoEncoder::Libx264,
            emits_progress: false,
        };
        self.run_generator(&encode, CLIP_ENCODE_TIMEOUT)?;
        publish_atomic(&staging, output, |candidate| {
            if fs::metadata(candidate).is_ok_and(|metadata| metadata.len() > 1024) {
                Ok(())
            } else {
                Err(MediaError::new(
                    "clip_empty",
                    "The generated clip was empty",
                ))
            }
        })?;
        // The frames were an intermediate; keeping them would double the cache for no benefit.
        // Retained on request, so a clip that came out wrong can be traced to generation or encode.
        if std::env::var_os("SOUNDAR_KEEP_CLIP_FRAMES").is_none() {
            let _ = fs::remove_dir_all(&frames_dir);
        }
        Ok(())
    }

    /// Run one generator or encoder command under a hard deadline.
    fn run_generator(&self, command: &RenderCommand, timeout: Duration) -> ServiceResult<()> {
        let mut child = command.command().spawn().map_err(|error| {
            VideoServiceError::io(
                "video.clip_failed",
                "The video generator could not start",
                error,
            )
        })?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    return Err(VideoServiceError::new(
                        "video.clip_failed",
                        "The clip could not be generated",
                    )
                    .details(json!({ "stderr": truncate_chars(stderr.trim(), 600) })));
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = terminate_process_group(&mut child, Duration::from_secs(5));
                    return Err(VideoServiceError::new(
                        "video.clip_failed",
                        "The clip generator exceeded its time budget",
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(200)),
                Err(error) => {
                    return Err(VideoServiceError::io(
                        "video.clip_failed",
                        "The clip generator could not be supervised",
                        error,
                    ))
                }
            }
        }
    }

    /// The id a generated cover claims for one exact card, so regenerating an unchanged episode
    /// finds its existing asset instead of stacking a second identical one.
    fn cover_asset_id(cache_key: &str) -> String {
        format!("cover-{}", &cache_key[..24])
    }

    /// Draw and attach this episode's cover, so an episode with nothing to look at still has a
    /// picture and can therefore be packaged as video.
    ///
    /// The card is derived from the episode - its name, its cast, its canvas - so it is stable,
    /// cacheable, and reproducible. It is registered as generated, never as user artwork, and it is
    /// placed underneath everything else so a real image added later simply covers it.
    pub fn ensure_episode_cover(
        &self,
        project_id: &str,
        actor: &str,
        redraw: bool,
    ) -> ServiceResult<Value> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;

        // An episode that already has something to look at is left alone. A drawn card is a floor
        // for episodes that have no picture, never a replacement for one the user chose.
        if !redraw && episode_has_own_picture(&manifest) {
            return Ok(record);
        }

        // A card is drawn at the canvas it will be composited onto, so it is never upscaled.
        let (width, height) = profile_dimensions(RenderProfile::Final, &manifest.layout);
        let spec = cover_spec(project_id, &manifest.name, &manifest.cast, width, height);
        let cache_key = spec.cache_key();
        let asset_id = Self::cover_asset_id(&cache_key);
        // An unchanged episode keeps the card it already has. Redrawing it would churn the
        // revision and invalidate renders for a file that would be byte-identical. A card whose
        // file has gone missing is not a card, so it is drawn again rather than left as a manifest
        // entry pointing at nothing.
        let existing = manifest
            .visual_assets
            .iter()
            .find(|asset| asset.id == asset_id);
        if !redraw {
            if let Some(existing) = existing {
                if self
                    .resolve_managed_path(&existing.managed_path)
                    .is_ok_and(|path| path.is_file())
                {
                    return Ok(record);
                }
            }
        }

        let span_us = self.episode_span_us(&manifest);
        if span_us <= 0 {
            return Err(VideoServiceError::new(
                "video.cover_span_unknown",
                "This episode has no duration yet, so a cover has nothing to cover",
            ));
        }

        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to draw a cover",
        )?;

        let output = self.cache_path("cover", &cache_key, "png")?;
        if !output.is_file() {
            let text_dir = self.video_root.join("cache").join("cover");
            self.secure_managed_directory(&text_dir)?;
            // Title and cast reach FFmpeg as files, never as filter arguments: a name containing a
            // comma or a quote is text, and must not be able to become filter syntax.
            let title_path = self.write_cover_text(&text_dir, &cache_key, "title", &spec.title)?;
            let subtitle_path = if spec.subtitle.is_empty() {
                None
            } else {
                Some(self.write_cover_text(&text_dir, &cache_key, "cast", &spec.subtitle)?)
            };
            let staging = sibling_staging_path(&output)?;
            let plan = build_cover_image_command(
                ffmpeg,
                &spec,
                &title_path,
                subtitle_path.as_deref(),
                &staging,
            )?;
            self.run_cover_plan(&plan)?;
            publish_atomic(&staging, &output, validate_image_file)?;
        } else {
            validate_image_file(&output)?;
        }

        let size_bytes = fs::metadata(&output)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.cover_failed",
                    "The generated cover could not be measured",
                    error,
                )
            })?
            .len();
        let sha256 = sha256_file(&output)?;
        let managed_path = self.relative_managed_path(&output)?;
        let created_at = utc_now();
        let asset = VisualAsset {
            id: asset_id.clone(),
            managed_path,
            sha256,
            mime_type: VisualMimeType::Png,
            width,
            height,
            has_alpha: false,
            size_bytes,
            provenance: Provenance {
                // Generated, and recorded as such: a drawn card must never be mistaken for
                // artwork the user supplied.
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: created_at.clone(),
                producer: "soundAr cover".to_string(),
                producer_version: Some(SERVICE_VERSION.to_string()),
                metadata: BTreeMap::from([
                    ("cache_key".to_string(), json!(cache_key)),
                    ("title".to_string(), json!(spec.title)),
                    ("cast_line".to_string(), json!(spec.subtitle)),
                ]),
            },
            created_at: created_at.clone(),
        };
        asset.validate()?;

        let full_canvas = NormalizedRect {
            x_bp: 0,
            y_bp: 0,
            width_bp: 10_000,
            height_bp: 10_000,
        };
        let layer = VisualLayer {
            id: format!("cover-layer-{}", &cache_key[..16]),
            asset_id,
            scene_id: None,
            range: TimeRange::new(0, span_us)?,
            fit: VisualFit::Cover,
            crop: None,
            // Underneath everything: a generated card is a floor, not a decision about the frame.
            z_index: -128,
            motion: VisualMotion {
                start_bounds: full_canvas,
                end_bounds: full_canvas,
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: VisualEasing::Linear,
            },
            transition_in_us: Microseconds::ZERO,
            transition_out_us: Microseconds::ZERO,
        };
        layer.validate()?;

        let expectation = project_expectation(&record)?;
        self.commit_manifest_mutation_at_if_parent_active(
            project_id,
            &expectation,
            actor,
            "Draw a cover for an episode with no picture",
            None,
            vec!["/visual_assets".to_string(), "/visual_layers".to_string()],
            BTreeSet::from([
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            None,
            move |manifest: &mut VideoProjectManifest| {
                // Replace any previous generated cover rather than accumulating one per rename.
                let stale = manifest
                    .visual_assets
                    .iter()
                    .filter(|existing| existing.id.starts_with("cover-"))
                    .map(|existing| existing.id.clone())
                    .collect::<Vec<_>>();
                manifest
                    .visual_layers
                    .retain(|existing| !stale.contains(&existing.asset_id));
                manifest
                    .visual_assets
                    .retain(|existing| !existing.id.starts_with("cover-"));
                manifest.visual_assets.push(asset.clone());
                manifest.visual_layers.push(layer.clone());
                Ok(())
            },
        )
    }

    /// How long this episode runs, measured from what has actually been placed on its clock.
    fn episode_span_us(&self, manifest: &VideoProjectManifest) -> i64 {
        let scene_end = manifest
            .reviewed_scenes
            .iter()
            .map(|scene| scene.timeline_start_us.0 + scene.timeline_duration_us.0)
            .max()
            .unwrap_or_default();
        let clip_end = manifest
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .map(|clip| clip.timeline_start_us.0 + clip.timeline_duration_us.0)
            .max()
            .unwrap_or_default();
        scene_end.max(clip_end)
    }

    /// Stage one piece of cover text as its own managed file.
    fn write_cover_text(
        &self,
        directory: &Path,
        cache_key: &str,
        role: &str,
        value: &str,
    ) -> ServiceResult<PathBuf> {
        let path = directory.join(format!("{cache_key}.{role}.txt"));
        if path.is_file() {
            return Ok(path);
        }
        let staging = sibling_staging_path(&path)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(PRIVATE_FILE_MODE)
            .open(&staging)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.cover_failed",
                    "The cover text could not be staged",
                    error,
                )
            })?;
        file.write_all(value.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                VideoServiceError::io(
                    "video.cover_failed",
                    "The cover text could not be written",
                    error,
                )
            })?;
        secure_managed_file(&staging)?;
        publish_atomic(&staging, &path, |candidate| {
            if fs::metadata(candidate).is_ok_and(|metadata| metadata.len() > 0) {
                Ok(())
            } else {
                Err(MediaError::new(
                    "cover_text_empty",
                    "The cover text was published empty",
                ))
            }
        })?;
        Ok(path)
    }

    /// Run a cover card render.
    ///
    /// A single frame with no encode has no progress to report and finishes in well under a
    /// second, so it runs directly under a hard deadline rather than through the job and progress
    /// machinery that exists for renders a user waits on.
    fn run_cover_plan(&self, plan: &RenderCommandPlan) -> ServiceResult<()> {
        let mut child = plan.primary.command().spawn().map_err(|error| {
            VideoServiceError::io(
                "video.cover_failed",
                "The cover render could not start",
                error,
            )
        })?;
        let deadline = Instant::now() + COVER_RENDER_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    return Err(VideoServiceError::new(
                        "video.cover_failed",
                        "The cover could not be drawn",
                    )
                    .details(json!({ "stderr": truncate_chars(stderr.trim(), 400) })));
                }
                Ok(None) if Instant::now() >= deadline => {
                    let _ = terminate_process_group(&mut child, Duration::from_secs(2));
                    return Err(VideoServiceError::new(
                        "video.cover_failed",
                        "The cover render exceeded its time budget",
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    return Err(VideoServiceError::io(
                        "video.cover_failed",
                        "The cover render could not be supervised",
                        error,
                    ))
                }
            }
        }
    }

    pub fn ensure_dialogue_scene(&self, project_id: &str, actor: &str) -> ServiceResult<Value> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        if manifest.dialogue.is_empty() {
            return Ok(record);
        }
        // soundAr owns the scene it generated and keeps it matching the performance; it never
        // rewrites divisions the author made. Once the episode has been divided deliberately, its
        // shape is the author's, and a re-narration must not flatten it back into one scene.
        let owns_the_only_scene = matches!(
            manifest.reviewed_scenes.as_slice(),
            [scene] if scene.id == DIALOGUE_SCENE_ID
        );
        if !manifest.reviewed_scenes.is_empty() && !owns_the_only_scene {
            return Ok(record);
        }
        let spoken_end = manifest
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.turn_id.is_some())
            .map(|clip| clip.timeline_start_us.0 + clip.timeline_duration_us.0)
            .max()
            .unwrap_or_default();
        if spoken_end <= 0 {
            return Err(VideoServiceError::new(
                "video.dialogue_not_performed",
                "Narrate the script before building its scene",
            ));
        }
        // A scene that already matches the performance is left exactly as it is. Committing an
        // identical scene would bump the revision and invalidate every render that depends on it.
        if manifest.reviewed_scenes.iter().any(|scene| {
            scene.id == DIALOGUE_SCENE_ID
                && scene.timeline_start_us == Microseconds::ZERO
                && scene.timeline_duration_us == Microseconds(spoken_end)
        }) && manifest.timeline_duration_us == Microseconds(spoken_end)
        {
            return Ok(record);
        }

        let expectation = project_expectation(&record)?;
        let script = manifest
            .dialogue
            .iter()
            .map(|turn| turn.spoken_text())
            .collect::<Vec<_>>()
            .join(" ");
        let title = manifest.name.clone();
        self.commit_manifest_mutation_at_if_parent_active(
            project_id,
            &expectation,
            actor,
            "Build the scene for a performed script",
            Some("ready"),
            vec![
                "/reviewed_scenes".to_string(),
                "/timeline_duration_us".to_string(),
            ],
            BTreeSet::from([
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::Tracking,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            None,
            move |manifest: &mut VideoProjectManifest| {
                // Replace rather than append: re-narrating a script must leave one scene that
                // matches the performance, not a second one stacked on the first.
                manifest
                    .reviewed_scenes
                    .retain(|scene| scene.id != DIALOGUE_SCENE_ID);
                manifest.reviewed_scenes.push(ReviewedScene {
                    id: DIALOGUE_SCENE_ID.to_string(),
                    candidate_id: None,
                    source_asset_id: None,
                    source_range: None,
                    timeline_start_us: Microseconds::ZERO,
                    timeline_duration_us: Microseconds(spoken_end),
                    title: title.clone(),
                    script: script.chars().take(90_000).collect(),
                    review_state: ReviewState::Approved,
                    revision: 1,
                });
                // An episode is as long as it was performed. Until now it carried the show
                // format's planning target, which is how long an episode of this show usually
                // runs - a target, never a measurement - so a 17-second conversation rendered as
                // ten minutes of silence with a picture held over it.
                manifest.timeline_duration_us = Microseconds(spoken_end);
                Ok(())
            },
        )
    }

    /// Measure the finished master's loudness.
    ///
    /// Returns `None` when there is no published master to measure or the analysis produced no
    /// usable number, so an unmeasured episode is never reported as being within target.
    pub fn measure_master_loudness(
        &self,
        project_id: &str,
    ) -> ServiceResult<Option<LoudnessMeasurement>> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        let Some(master) = manifest.render_artifacts.iter().find(|artifact| {
            matches!(artifact.role, RenderArtifactRole::FinalMaster)
                && matches!(artifact.publication_state, PublicationState::Published)
        }) else {
            return Ok(None);
        };
        let master_path =
            self.resolve_absolute_managed_path(&self.video_root.join(&master.managed_path))?;
        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to measure the master",
        )?;
        let command = build_loudness_analysis_command(ffmpeg, &master_path)?;
        let output = command.command().output().map_err(|error| {
            VideoServiceError::io(
                "video.loudness_analysis_failed",
                "The master could not be measured",
                error,
            )
        })?;
        if !output.status.success() {
            return Err(VideoServiceError::new(
                "video.loudness_analysis_failed",
                "The loudness analysis did not complete",
            ));
        }
        Ok(parse_loudness_analysis(&String::from_utf8_lossy(
            &output.stderr,
        )))
    }

    /// Check a rendered episode against the script it was asked to speak.
    ///
    /// `heard` maps a turn id to what a local recognizer actually heard in that turn's take. A turn
    /// missing from that map is reported as unchecked rather than as passed, because nobody
    /// listened back to it. `loudness` is a measurement the runtime made; without one the report
    /// says the master was not checked instead of claiming it is within target.
    ///
    /// This reports. It never rewrites a script, re-renders a take, or adjusts a mix.
    pub fn check_episode_quality(
        &self,
        project_id: &str,
        heard: &BTreeMap<String, String>,
        loudness: Option<LoudnessMeasurement>,
    ) -> ServiceResult<QcReport> {
        // Measuring the master here is what turns "unchecked" into a real answer. A caller may
        // still supply a measurement it made itself; nothing is invented when neither exists.
        let loudness = match loudness {
            Some(measured) => Some(measured),
            None => self.measure_master_loudness(project_id).unwrap_or(None),
        };
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;

        let mut findings = Vec::new();
        let mut checked_turns = Vec::new();
        let mut unchecked_turns = Vec::new();

        // Only a turn with a take can be listened back to; a turn nobody narrated is not a turn
        // that failed a check, it is a turn with no audio to check.
        let narrated_turns = manifest
            .narration_bindings
            .iter()
            .filter_map(|binding| binding.turn_id.as_deref())
            .collect::<BTreeSet<_>>();
        for turn in &manifest.dialogue {
            if !narrated_turns.contains(turn.id.as_str()) {
                continue;
            }
            match heard.get(&turn.id) {
                Some(spoken) => {
                    // The comparison is against what the voice was asked to say, which is the
                    // scripted line after its pronunciation rules were applied.
                    let asked = apply_lexicon(
                        turn.spoken_text(),
                        &effective_entries(&manifest.lexicon, &turn.character_id),
                    )
                    .spoken_text;
                    findings.extend(findings_for_turn(
                        &turn.id,
                        &diff_spoken_words(&asked, spoken),
                    ));
                    checked_turns.push(turn.id.clone());
                }
                None => unchecked_turns.push(turn.id.clone()),
            }
        }

        if let Some(measured) = loudness {
            findings.extend(findings_for_loudness(
                measured,
                manifest.audio_mix.target_lufs_milli,
                manifest.audio_mix.true_peak_db_milli,
            ));
        }

        // Silence a listener actually hears: the gaps between what the speech track occupies.
        let speech_spans = manifest
            .tracks
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::Audio))
            .flat_map(|track| track.clips.iter())
            .filter(|clip| clip.turn_id.is_some())
            .map(|clip| {
                Ok((
                    clip.timeline_start_us,
                    clip.timeline_start_us
                        .checked_add(clip.timeline_duration_us)?,
                ))
            })
            .collect::<Result<Vec<_>, VideoError>>()?;
        if !speech_spans.is_empty() {
            findings.extend(findings_for_dead_air(
                &speech_spans,
                manifest.timeline_duration_us,
            ));
        }

        Ok(build_report(
            findings,
            checked_turns,
            unchecked_turns,
            loudness.is_some(),
        )?)
    }

    /// Render one deliverable, publish it atomically, and measure what was actually produced.
    ///
    /// The validator runs against the published file rather than the staged one, so a deliverable
    /// only becomes addressable after it has been proved to be the media it claims to be.
    #[allow(clippy::too_many_arguments)]
    fn render_release_member(
        &self,
        job_id: &str,
        project_id: &str,
        plan: RenderCommandPlan,
        output: &Path,
        expected_duration_us: Option<i64>,
        ffprobe: &Path,
        cancel: &AtomicBool,
        playable: impl Fn(&RuntimeMediaProbe) -> bool,
        rejection: &'static str,
    ) -> ServiceResult<RenderedRelease> {
        let staging = plan.primary.output.clone();
        if output.is_file() {
            fs::remove_file(output).map_err(|error| {
                VideoServiceError::io(
                    "video.release_failed",
                    "A previous deliverable could not be cleared",
                    error,
                )
            })?;
        }
        self.execute_render_plan(
            job_id,
            project_id,
            &plan,
            expected_duration_us,
            0.1,
            0.9,
            cancel,
            None,
        )?;
        publish_atomic(&staging, output, |path| {
            let probe = probe_media(path, ffprobe)?;
            if probe.duration_us <= 0 || !playable(&probe) {
                return Err(MediaError::new("invalid_release_member", rejection));
            }
            Ok(())
        })?;
        let probe = probe_media(output, ffprobe)?;
        let stream = probe
            .primary_video_stream
            .and_then(|index| probe.streams.iter().find(|stream| stream.index == index));
        Ok(RenderedRelease {
            sha256: sha256_file(output)?,
            duration_us: probe.duration_us,
            width: stream.and_then(|stream| stream.width),
            height: stream.and_then(|stream| stream.height),
        })
    }

    fn release_artifact(
        &self,
        role: RenderArtifactRole,
        path: &Path,
        rendered: &RenderedRelease,
        mime_type: &str,
    ) -> ServiceResult<RenderArtifact> {
        let artifact = RenderArtifact {
            id: new_id(),
            role,
            scene_id: None,
            managed_path: self.relative_managed_path(path)?,
            sha256: rendered.sha256.clone(),
            // A deliverable is addressed by its own bytes: it is derived from one exact master and
            // is replaced wholesale whenever that master is re-rendered.
            cache_key: rendered.sha256.clone(),
            mime_type: mime_type.to_string(),
            duration_us: Some(Microseconds(rendered.duration_us)),
            width: rendered.width,
            height: rendered.height,
            publication_state: PublicationState::Published,
            created_at: utc_now(),
        };
        artifact.validate()?;
        Ok(artifact)
    }

    fn commit_release_artifacts(
        &self,
        project_id: &str,
        actor: &str,
        manifest: VideoProjectManifest,
        job_id: &str,
    ) -> ServiceResult<Value> {
        let lock = ProjectLock::acquire(self, project_id, actor)?;
        let current = self.get_project(project_id)?;
        let revision = current
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_store_shape("revision"))?;
        let mut manifest = manifest;
        let next_revision = manifest.revision.checked_add(1).ok_or_else(|| {
            VideoServiceError::new(
                "video.revision_overflow",
                "The release revision could not be advanced",
            )
        })?;
        manifest.revision = next_revision;
        manifest.updated_at = utc_now();
        let reason = "Publish release deliverables".to_string();
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: next_revision,
            parent_id: manifest
                .revision_history
                .last()
                .map(|record| record.id.clone()),
            actor: actor.to_string(),
            reason: reason.clone(),
            changed_paths: vec!["/render_artifacts".to_string()],
            invalidated_stages: BTreeSet::from([RevisionStage::PublishPackage]),
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict()?;
        let manifest_value = serde_json::to_value(&manifest).map_err(json_error)?;
        let status = current.get("status").and_then(Value::as_str);
        let project = self
            .store
            .commit_video_manifest_and_complete_job(
                project_id,
                revision,
                &manifest_value,
                actor,
                &reason,
                &lock.token,
                status,
                job_id,
            )
            .map_err(VideoServiceError::store)?;
        drop(lock);
        Ok(project)
    }

    /// Produce and register every release deliverable this episode can supply.
    ///
    /// All three are derived from the finished master, so the master is the one hard prerequisite.
    /// A member that cannot be produced is reported with its reason rather than omitted, because a
    /// partial release that looks complete is worse than one that says what is missing.
    pub fn export_episode_release(
        &self,
        project_id: &str,
        actor: &str,
        has_show_notes: bool,
    ) -> ServiceResult<ReleaseExportResult> {
        let plan = self.plan_episode_release(project_id, has_show_notes)?;
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;

        // A stand-in must never be published. The manifest already refuses a master built on one,
        // but saying so here names the actual problem instead of failing later on a checksum.
        let drafts = manifest.draft_turn_ids();
        if !drafts.is_empty() {
            return Err(VideoServiceError::new(
                "video.draft_not_promoted",
                format!(
                    "{} line(s) are still draft takes; promote them before publishing a release",
                    drafts.len()
                ),
            ));
        }

        let master = manifest
            .render_artifacts
            .iter()
            .find(|artifact| {
                matches!(artifact.role, RenderArtifactRole::FinalMaster)
                    && matches!(artifact.publication_state, PublicationState::Published)
            })
            .cloned()
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.final_master_required",
                    "Render a final master before exporting a release",
                )
            })?;
        // `managed_path` is relative to managed video storage; the resolver takes an absolute path
        // and re-checks that it is still inside that storage.
        let master_path =
            self.resolve_absolute_managed_path(&self.video_root.join(&master.managed_path))?;

        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to produce release deliverables",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate release deliverables",
        )?;

        let request_value = json!({ "project_id": project_id, "has_show_notes": has_show_notes });
        let job_id = self
            .store
            .create_job("video_export_release", &request_value)
            .map_err(VideoServiceError::store)?;
        let result = self.export_release_deliverables(
            &job_id,
            project_id,
            actor,
            manifest.clone(),
            plan,
            &master,
            &master_path,
            ffmpeg,
            ffprobe,
        );
        if let Err(error) = &result {
            let _ = self.store.fail_job(&job_id, &error.stable_message());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn export_release_deliverables(
        &self,
        job_id: &str,
        project_id: &str,
        actor: &str,
        manifest: VideoProjectManifest,
        plan: ReleasePlan,
        master: &RenderArtifact,
        master_path: &Path,
        ffmpeg: &Path,
        ffprobe: &Path,
    ) -> ServiceResult<ReleaseExportResult> {
        self.store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        let cancel = AtomicBool::new(false);
        let _lease = self.acquire_resources(
            job_id,
            project_id,
            ResourceRequest {
                class: ResourceClass::Medium,
                vram_mb: 0,
                cpu_threads: 4,
                io_slots: 2,
                nvenc_sessions: 0,
            },
            &cancel,
            None,
        )?;

        let deliverables_dir = self.project_dir(project_id)?.join("release");
        self.secure_managed_directory(&deliverables_dir)?;
        let mut produced = Vec::new();
        let mut artifacts = Vec::new();

        // The audio episode, carrying the chapters the author's own scenes define.
        let chapters_path = deliverables_dir.join("chapters.ffmetadata");
        // The chapter document is written through the same atomic path as any other managed file,
        // so a crash mid-write cannot leave a truncated document that FFmpeg would still parse.
        let chapters_staging = sibling_staging_path(&chapters_path)?;
        fs::write(&chapters_staging, ffmetadata_chapters(&plan.chapters)).map_err(|error| {
            VideoServiceError::io(
                "video.release_failed",
                "The chapter metadata could not be staged",
                error,
            )
        })?;
        publish_atomic(&chapters_staging, &chapters_path, |path| {
            fs::read_to_string(path)
                .map_err(|error| {
                    MediaError::new(
                        "invalid_chapter_metadata",
                        "The chapter metadata could not be read back",
                    )
                    .detail(error.to_string())
                })
                .and_then(|document| {
                    document
                        .starts_with(";FFMETADATA1")
                        .then_some(())
                        .ok_or_else(|| {
                            MediaError::new(
                                "invalid_chapter_metadata",
                                "The chapter metadata is not an FFmetadata document",
                            )
                        })
                })
        })?;
        let podcast_path = deliverables_dir.join("episode.m4a");
        let podcast = self.render_release_member(
            job_id,
            project_id,
            build_podcast_audio_command(
                ffmpeg,
                master_path,
                (!plan.chapters.is_empty()).then_some(chapters_path.as_path()),
                &sibling_staging_path(&podcast_path)?,
            )?,
            &podcast_path,
            master.duration_us.map(|duration| duration.0),
            ffprobe,
            &cancel,
            |probe| probe.primary_audio_stream.is_some() && probe.primary_video_stream.is_none(),
            "The podcast deliverable is not audio-only playable media",
        )?;
        artifacts.push(self.release_artifact(
            RenderArtifactRole::PodcastAudio,
            &podcast_path,
            &podcast,
            "audio/mp4",
        )?);
        produced.push(ReleaseMemberArtifact {
            kind: ReleaseMemberKind::PodcastAudio,
            artifact_id: artifacts.last().expect("just pushed").id.clone(),
            managed_path: artifacts.last().expect("just pushed").managed_path.clone(),
            sha256: artifacts.last().expect("just pushed").sha256.clone(),
            mime_type: "audio/mp4".to_string(),
            duration_us: podcast.duration_us,
        });

        // The trailer, cut from the moment the analyst chose in the episode's own narration.
        if let Some(range) = plan.trailer_range {
            let trailer_path = deliverables_dir.join("trailer.mp4");
            let trailer = self.render_release_member(
                job_id,
                project_id,
                build_trailer_command(
                    ffmpeg,
                    master_path,
                    &sibling_staging_path(&trailer_path)?,
                    range.start_us.0,
                    range.end_us.0,
                    RenderProfile::Final,
                    self.runtime_status(false).h264_nvenc_runtime,
                )?,
                &trailer_path,
                Some(range.end_us.0 - range.start_us.0),
                ffprobe,
                &cancel,
                |probe| probe.primary_video_stream.is_some(),
                "The trailer is not playable video",
            )?;
            artifacts.push(self.release_artifact(
                RenderArtifactRole::Trailer,
                &trailer_path,
                &trailer,
                "video/mp4",
            )?);
            produced.push(ReleaseMemberArtifact {
                kind: ReleaseMemberKind::Trailer,
                artifact_id: artifacts.last().expect("just pushed").id.clone(),
                managed_path: artifacts.last().expect("just pushed").managed_path.clone(),
                sha256: artifacts.last().expect("just pushed").sha256.clone(),
                mime_type: "video/mp4".to_string(),
                duration_us: trailer.duration_us,
            });
        }

        // The audiogram, for feeds where only video plays.
        let audiogram_path = deliverables_dir.join("audiogram.mp4");
        let audiogram = self.render_release_member(
            job_id,
            project_id,
            build_audiogram_command(
                ffmpeg,
                master_path,
                &sibling_staging_path(&audiogram_path)?,
                RenderProfile::Preview,
                self.runtime_status(false).h264_nvenc_runtime,
            )?,
            &audiogram_path,
            master.duration_us.map(|duration| duration.0),
            ffprobe,
            &cancel,
            |probe| probe.primary_video_stream.is_some() && probe.primary_audio_stream.is_some(),
            "The audiogram is not playable audio/video",
        )?;
        artifacts.push(self.release_artifact(
            RenderArtifactRole::Audiogram,
            &audiogram_path,
            &audiogram,
            "video/mp4",
        )?);
        produced.push(ReleaseMemberArtifact {
            kind: ReleaseMemberKind::Audiogram,
            artifact_id: artifacts.last().expect("just pushed").id.clone(),
            managed_path: artifacts.last().expect("just pushed").managed_path.clone(),
            sha256: artifacts.last().expect("just pushed").sha256.clone(),
            mime_type: "video/mp4".to_string(),
            duration_us: audiogram.duration_us,
        });

        // Commit every deliverable as one revision, so a release is either registered whole or not
        // at all rather than leaving half its members addressable.
        // Replacing rather than appending: a release is derived wholly from one master, so leaving
        // a previous export's members addressable would let two different episodes be downloaded
        // from the same project.
        let mut revised = manifest;
        revised
            .render_artifacts
            .retain(|artifact| !artifact.role.is_release_member() || artifact.id == master.id);
        revised.render_artifacts.extend(artifacts);
        let project = self.commit_release_artifacts(project_id, actor, revised, job_id)?;

        Ok(ReleaseExportResult {
            project,
            produced,
            skipped: plan
                .members
                .into_iter()
                .filter(|member| !member.ready)
                .collect(),
            job_id: job_id.to_string(),
        })
    }

    /// What this episode's release would contain, and what is still missing.
    ///
    /// The trailer moment is chosen by pointing soundAr's existing candidate analyst at the
    /// episode's own narration, so generated work is reviewed by the same deterministic rules as
    /// imported source rather than by a second, unproven selector.
    pub fn plan_episode_release(
        &self,
        project_id: &str,
        has_show_notes: bool,
    ) -> ServiceResult<ReleasePlan> {
        let record = self.get_project(project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;

        let trailer_range = match episode_transcript(&manifest)? {
            Some(transcript) => identify_clip_candidates(
                &transcript,
                &CandidatePolicy {
                    minimum_duration_us: Microseconds(TRAILER_MINIMUM_US),
                    target_duration_us: Microseconds(TRAILER_TARGET_US),
                    maximum_duration_us: Microseconds(TRAILER_MAXIMUM_US),
                    maximum_candidates: 4,
                },
                &BTreeSet::new(),
            )?
            .candidates
            .first()
            .map(|candidate| candidate.source_range),
            None => None,
        };
        Ok(plan_release(&manifest, trailer_range, has_show_notes)?)
    }

    /// Every saved show format, validated on the way out so a corrupted document cannot reach a
    /// project as if it were a usable format.
    pub fn list_show_formats(&self) -> ServiceResult<Vec<ShowFormat>> {
        let stored = self
            .store
            .show_formats()
            .map_err(VideoServiceError::store)?;
        let formats: Vec<ShowFormat> = serde_json::from_value(stored).map_err(json_error)?;
        for format in &formats {
            format.validate()?;
        }
        Ok(formats)
    }

    /// Create or replace one show format, advancing its revision.
    ///
    /// The revision is owned here rather than by the caller: an episode records the revision it
    /// inherited from, and a caller that could choose its own number could make two different
    /// formats claim the same provenance.
    pub fn save_show_format(&self, mut format: ShowFormat) -> ServiceResult<ShowFormat> {
        let mut formats = self.list_show_formats()?;
        let existing = formats.iter().position(|saved| saved.id == format.id);
        format.revision = existing
            .and_then(|index| formats.get(index))
            .map_or(1, |saved| saved.revision.saturating_add(1));
        format.created_at = existing
            .and_then(|index| formats.get(index))
            .map_or_else(utc_now, |saved| saved.created_at.clone());
        format.updated_at = utc_now();
        format.validate()?;
        match existing {
            Some(index) => formats[index] = format.clone(),
            None => {
                if formats.len() >= MAX_SHOW_FORMATS {
                    return Err(VideoServiceError::new(
                        "video.too_many_show_formats",
                        format!("soundAr keeps at most {MAX_SHOW_FORMATS} show formats"),
                    ));
                }
                formats.push(format.clone());
            }
        }
        let value = serde_json::to_value(&formats).map_err(json_error)?;
        self.store
            .save_show_formats(&value)
            .map_err(VideoServiceError::store)?;
        Ok(format)
    }

    pub fn delete_show_format(&self, format_id: &str) -> ServiceResult<()> {
        let mut formats = self.list_show_formats()?;
        let before = formats.len();
        formats.retain(|format| format.id != format_id);
        if formats.len() == before {
            return Err(VideoServiceError::new(
                "video.show_format_not_found",
                "That show format does not exist",
            ));
        }
        let value = serde_json::to_value(&formats).map_err(json_error)?;
        self.store
            .save_show_formats(&value)
            .map_err(VideoServiceError::store)
    }

    /// Start a new episode of a show.
    ///
    /// The episode inherits the format's decisions by copy. Nothing links back, so editing the
    /// format later cannot change this episode, and this episode reproduces what it was made from.
    pub fn create_episode(
        &self,
        format_id: &str,
        episode_name: &str,
        actor: &str,
        initial_intent: Option<String>,
    ) -> ServiceResult<Value> {
        let format = self
            .list_show_formats()?
            .into_iter()
            .find(|format| format.id == format_id)
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.show_format_not_found",
                    "That show format does not exist",
                )
            })?;
        let project_id = format!("project-{}", new_id());
        let manifest = instantiate_format(&format, &project_id, episode_name, &utc_now())?;
        self.create_project(CreateVideoProjectRequest {
            name: episode_name.to_string(),
            manifest,
            actor: actor.to_string(),
            initial_intent,
        })
    }

    /// Records one exact local image selected by the trusted native backend picker. The returned
    /// opaque receipt is short-lived and may be claimed by only one durable add-visual job.
    pub(crate) fn authorize_user_visual_selection(
        &self,
        request: AuthorizeVisualSelectionRequest,
        source_path: PathBuf,
    ) -> ServiceResult<VisualSourceReceipt> {
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(
            &current,
            &ProjectExpectation {
                revision: request.expected_revision,
                version_id: request.expected_version_id.clone(),
            },
        )?;
        let (identity, inspection, sha256) = inspect_exact_visual_source(&source_path)?;
        let issued_at = Utc::now();
        let receipt = json!({
            "id": format!("visual-selection-{}", new_id()),
            "receipt_kind": "user_selected",
            "project_id": request.project_id,
            "expected_revision": request.expected_revision,
            "expected_version_id": request.expected_version_id,
            "source_path": source_path.to_str().ok_or_else(|| VideoServiceError::new(
                "video.invalid_visual_source",
                "Selected visual paths must be valid UTF-8",
            ))?,
            "source_device": identity.device.to_string(),
            "source_inode": identity.inode.to_string(),
            "size_bytes": identity.size,
            "modified_seconds": identity.modified_seconds,
            "modified_nanoseconds": identity.modified_nanoseconds,
            "sha256": sha256,
            "mime_type": inspection.mime_type.as_mime(),
            "width": inspection.width,
            "height": inspection.height,
            "has_alpha": inspection.has_alpha,
            "producer": "soundAr native file picker",
            "producer_version": env!("CARGO_PKG_VERSION"),
            "generation_id": Value::Null,
            "trust_context": {
                "boundary": "native_file_picker",
                "single_file": true,
            },
            "issued_at": issued_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "expires_at": (issued_at + ChronoDuration::minutes(VISUAL_SOURCE_RECEIPT_TTL_MINUTES))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        });
        let saved = self
            .store
            .create_video_visual_source_receipt(&receipt)
            .map_err(VideoServiceError::store)?;
        visual_source_receipt(&saved)
    }

    /// Registers an image-generation result whose path and identity came from the authenticated
    /// Codex app-server, not from model tool arguments. Registration copies the exact bytes into
    /// private managed storage before minting the one-use receipt consumed by add_visual_asset.
    pub(crate) fn register_trusted_generated_visual(
        &self,
        request: AuthorizeVisualSelectionRequest,
        generation: TrustedGeneratedVisual,
    ) -> ServiceResult<VisualSourceReceipt> {
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(
            &current,
            &ProjectExpectation {
                revision: request.expected_revision,
                version_id: request.expected_version_id.clone(),
            },
        )?;
        let thread_id = require_text(
            &generation.thread_id,
            "video.invalid_generation_receipt",
            "The authenticated generation thread is required",
        )?;
        let turn_id = require_text(
            &generation.turn_id,
            "video.invalid_generation_receipt",
            "The authenticated generation turn is required",
        )?;
        let generation_id = require_text(
            &generation.generation_id,
            "video.invalid_generation_receipt",
            "The authenticated generation item is required",
        )?;
        let producer = "OpenAI Codex image generation";
        let receipt_id = format!(
            "visual-generation-{}",
            sha256_bytes(
                json!([request.project_id, thread_id, turn_id, generation_id])
                    .to_string()
                    .as_bytes(),
            )
        );
        if let Some(existing) = self
            .store
            .get_video_visual_source_receipt(&receipt_id)
            .map_err(VideoServiceError::store)?
        {
            let exact = existing.get("receipt_kind").and_then(Value::as_str)
                == Some("generated_locally")
                && existing.get("project_id").and_then(Value::as_str)
                    == Some(request.project_id.as_str())
                && existing.get("expected_revision").and_then(Value::as_i64)
                    == Some(request.expected_revision)
                && existing.get("expected_version_id").and_then(Value::as_str)
                    == Some(request.expected_version_id.as_str())
                && existing.get("generation_id").and_then(Value::as_str) == Some(generation_id)
                && existing
                    .pointer("/trust_context/thread_id")
                    .and_then(Value::as_str)
                    == Some(thread_id)
                && existing
                    .pointer("/trust_context/turn_id")
                    .and_then(Value::as_str)
                    == Some(turn_id);
            if !exact {
                return Err(VideoServiceError::new(
                    "video.generation_identity_conflict",
                    "The authenticated generation identity is already bound differently",
                ));
            }
            let path = PathBuf::from(value_string(&existing, "source_path")?);
            secure_managed_file(&path)?;
            let expected_identity = LocalSourceIdentity {
                device: value_string(&existing, "source_device")?
                    .parse()
                    .map_err(|_| invalid_store_shape("source_device"))?,
                inode: value_string(&existing, "source_inode")?
                    .parse()
                    .map_err(|_| invalid_store_shape("source_inode"))?,
                size: u64::try_from(value_i64(&existing, "size_bytes")?)
                    .map_err(|_| invalid_store_shape("size_bytes"))?,
                modified_seconds: value_i64(&existing, "modified_seconds")?,
                modified_nanoseconds: value_i64(&existing, "modified_nanoseconds")?,
            };
            let (actual_identity, inspection, actual_sha256) = inspect_exact_visual_source(&path)?;
            if actual_identity != expected_identity
                || actual_sha256 != value_string(&existing, "sha256")?
                || inspection.mime_type.as_mime() != value_string(&existing, "mime_type")?.as_str()
                || inspection.width
                    != u32::try_from(value_i64(&existing, "width")?)
                        .map_err(|_| invalid_store_shape("width"))?
                || inspection.height
                    != u32::try_from(value_i64(&existing, "height")?)
                        .map_err(|_| invalid_store_shape("height"))?
                || existing.get("has_alpha").and_then(Value::as_bool) != Some(inspection.has_alpha)
            {
                return Err(VideoServiceError::new(
                    "video.generation_identity_conflict",
                    "The registered generation bytes no longer match their receipt",
                ));
            }
            return visual_source_receipt(&existing);
        }

        let (source_identity, inspection, source_sha256) =
            inspect_exact_visual_source(&generation.source_path)?;
        let extension = visual_extension(inspection.mime_type);
        let generated_dir = self.video_root.join("registered-generations");
        self.secure_managed_directory(&generated_dir)?;
        let final_path = generated_dir.join(format!("{receipt_id}.{extension}"));
        let staging_path =
            generated_dir.join(format!(".{receipt_id}.{}.partial", Uuid::new_v4().simple()));
        let staged_sha = copy_file_cancelable(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&generation.source_path)
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.visual_not_found",
                        "The authenticated generation output could not be reopened",
                        error,
                    )
                })?,
            source_identity,
            &staging_path,
            &AtomicBool::new(false),
            |_| {},
        )?;
        if staged_sha != source_sha256 || inspect_image_file(&staging_path)? != inspection {
            let _ = fs::remove_file(&staging_path);
            return Err(VideoServiceError::new(
                "video.visual_changed",
                "The authenticated generation output changed during registration",
            ));
        }
        match fs::hard_link(&staging_path, &final_path) {
            Ok(()) => {
                fs::remove_file(&staging_path).map_err(|error| {
                    VideoServiceError::io(
                        "video.import_cleanup_failed",
                        "The generation staging link could not be finalized",
                        error,
                    )
                })?;
                secure_managed_file(&final_path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&staging_path);
                secure_managed_file(&final_path)?;
                if sha256_file_with_cancel(&final_path, None)? != source_sha256
                    || inspect_image_file(&final_path)? != inspection
                {
                    return Err(VideoServiceError::new(
                        "video.generation_identity_conflict",
                        "The generation identity already owns different managed bytes",
                    ));
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&staging_path);
                return Err(VideoServiceError::io(
                    "video.publication_commit_failed",
                    "The generation output could not be registered atomically",
                    error,
                ));
            }
        }
        let (managed_identity, managed_inspection, managed_sha256) =
            inspect_exact_visual_source(&final_path)?;
        if managed_sha256 != source_sha256 || managed_inspection != inspection {
            return Err(VideoServiceError::new(
                "video.generation_identity_conflict",
                "The registered generation bytes failed final verification",
            ));
        }
        let issued_at = Utc::now();
        let revised_prompt_sha256 = generation
            .revised_prompt
            .as_deref()
            .map(|prompt| sha256_bytes(prompt.as_bytes()));
        let receipt = json!({
            "id": receipt_id,
            "receipt_kind": "generated_locally",
            "project_id": request.project_id,
            "expected_revision": request.expected_revision,
            "expected_version_id": request.expected_version_id,
            "source_path": final_path,
            "source_device": managed_identity.device.to_string(),
            "source_inode": managed_identity.inode.to_string(),
            "size_bytes": managed_identity.size,
            "modified_seconds": managed_identity.modified_seconds,
            "modified_nanoseconds": managed_identity.modified_nanoseconds,
            "sha256": managed_sha256,
            "mime_type": managed_inspection.mime_type.as_mime(),
            "width": managed_inspection.width,
            "height": managed_inspection.height,
            "has_alpha": managed_inspection.has_alpha,
            "producer": producer,
            "producer_version": generation.producer_version,
            "generation_id": generation_id,
            "trust_context": {
                "boundary": "codex_app_server",
                "thread_id": thread_id,
                "turn_id": turn_id,
                "item_id": generation_id,
                "revised_prompt_sha256": revised_prompt_sha256,
            },
            "issued_at": issued_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "expires_at": (issued_at + ChronoDuration::minutes(VISUAL_SOURCE_RECEIPT_TTL_MINUTES))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        });
        let saved = self
            .store
            .create_video_visual_source_receipt(&receipt)
            .map_err(VideoServiceError::store)?;
        visual_source_receipt(&saved)
    }

    fn claim_visual_source(
        &self,
        request: &AddVisualAssetRequest,
        job_id: &str,
    ) -> ServiceResult<ResolvedVisualSource> {
        let receipt_id = require_text(
            request.origin.receipt_id(),
            "video.approval_required",
            "A trusted visual source receipt is required",
        )?;
        if receipt_id.chars().count() > 160 {
            return Err(VideoServiceError::new(
                "video.approval_required",
                "The visual source receipt is invalid",
            ));
        }
        let receipt = self
            .store
            .claim_video_visual_source_receipt(
                receipt_id,
                request.origin.receipt_kind(),
                &request.project_id,
                request.expected_revision,
                &request.expected_version_id,
                job_id,
            )
            .map_err(VideoServiceError::store)?;
        let raw_path = PathBuf::from(value_string(&receipt, "source_path")?);
        let path = if matches!(request.origin, VisualAssetOrigin::GeneratedLocally { .. }) {
            secure_managed_file(&raw_path)?;
            self.resolve_absolute_managed_path(&raw_path)?
        } else {
            raw_path
        };
        let expected_identity = LocalSourceIdentity {
            device: value_string(&receipt, "source_device")?
                .parse()
                .map_err(|_| invalid_store_shape("source_device"))?,
            inode: value_string(&receipt, "source_inode")?
                .parse()
                .map_err(|_| invalid_store_shape("source_inode"))?,
            size: u64::try_from(value_i64(&receipt, "size_bytes")?)
                .map_err(|_| invalid_store_shape("size_bytes"))?,
            modified_seconds: value_i64(&receipt, "modified_seconds")?,
            modified_nanoseconds: value_i64(&receipt, "modified_nanoseconds")?,
        };
        let expected_mime = match value_string(&receipt, "mime_type")?.as_str() {
            "image/png" => VisualMimeType::Png,
            "image/jpeg" => VisualMimeType::Jpeg,
            "image/webp" => VisualMimeType::Webp,
            _ => return Err(invalid_store_shape("mime_type")),
        };
        let expected_inspection = ImageInspection {
            mime_type: expected_mime,
            width: u32::try_from(value_i64(&receipt, "width")?)
                .map_err(|_| invalid_store_shape("width"))?,
            height: u32::try_from(value_i64(&receipt, "height")?)
                .map_err(|_| invalid_store_shape("height"))?,
            has_alpha: receipt
                .get("has_alpha")
                .and_then(Value::as_bool)
                .ok_or_else(|| invalid_store_shape("has_alpha"))?,
            size_bytes: expected_identity.size,
        };
        let expected_sha256 = value_string(&receipt, "sha256")?;
        let (actual_identity, actual_inspection, actual_sha256) =
            inspect_exact_visual_source(&path)?;
        if actual_identity != expected_identity
            || actual_inspection != expected_inspection
            || actual_sha256 != expected_sha256
        {
            return Err(VideoServiceError::new(
                "video.visual_receipt_mismatch",
                "The authorized visual source no longer matches its exact-file receipt",
            ));
        }
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "authorization_receipt_id".to_string(),
            Value::String(receipt_id.to_string()),
        );
        metadata.insert(
            "source_sha256".to_string(),
            Value::String(expected_sha256.clone()),
        );
        if let Some(generation_id) = receipt.get("generation_id").and_then(Value::as_str) {
            metadata.insert(
                "generation_id".to_string(),
                Value::String(generation_id.to_string()),
            );
        }
        if let Some(trust_context) = receipt
            .get("trust_context")
            .filter(|value| value.is_object())
        {
            metadata.insert("trust_context".to_string(), trust_context.clone());
        }
        let provenance = Provenance {
            kind: match request.origin {
                VisualAssetOrigin::UserSelected { .. } => ProvenanceKind::UserUpload,
                VisualAssetOrigin::GeneratedLocally { .. } => ProvenanceKind::GeneratedLocally,
            },
            original_uri: None,
            imported_at: utc_now(),
            producer: value_string(&receipt, "producer")?,
            producer_version: receipt
                .get("producer_version")
                .and_then(Value::as_str)
                .map(str::to_string),
            metadata,
        };
        Ok(ResolvedVisualSource {
            path,
            expected_identity,
            expected_sha256,
            inspection: expected_inspection,
            provenance,
        })
    }

    /// Imports one user-selected or locally generated still and places it on the canonical
    /// project clock. The durable operation owns deterministic asset/layer identities, a private
    /// no-clobber managed copy, project-version CAS, and terminal completion.
    pub fn add_visual_asset(
        &self,
        request: AddVisualAssetRequest,
    ) -> ServiceResult<AddVisualAssetResult> {
        let actor = require_text(
            &request.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        let operation_id = require_text(
            &request.operation_id,
            "video.invalid_operation_id",
            "A visual operation identifier is required",
        )?;
        if operation_id.chars().count() > 256 {
            return Err(VideoServiceError::new(
                "video.invalid_operation_id",
                "The visual operation identifier is too long",
            ));
        }
        request.range.validate()?;
        request.motion.validate()?;
        if request.origin.receipt_id().trim().is_empty() {
            return Err(VideoServiceError::new(
                "video.approval_required",
                "Importing a visual requires a trusted exact-file receipt",
            ));
        }
        let request_value = serde_json::to_value(&request).map_err(json_error)?;
        let idempotency_key = format!(
            "visual-add:{}",
            sha256_bytes(format!("{}:{operation_id}", request.project_id).as_bytes())
        );
        let Some((job_id, created)) = self
            .store
            .create_idempotent_job("video_add_visual_asset", &idempotency_key, &request_value)
            .map_err(VideoServiceError::store)?
        else {
            return Err(VideoServiceError::new(
                "video.idempotency_conflict",
                "This visual operation identifier was already used with a different request",
            ));
        };
        let result = self.add_visual_asset_with_job(&request, &job_id, created, actor);
        if let Err(error) = &result {
            let _ = self.store.fail_job(&job_id, &error.stable_message());
        }
        result
    }

    fn add_visual_asset_with_job(
        &self,
        request: &AddVisualAssetRequest,
        job_id: &str,
        created: bool,
        actor: &str,
    ) -> ServiceResult<AddVisualAssetResult> {
        let asset_id =
            stable_import_asset_id(&request.project_id, job_id, "visual", &request.operation_id);
        let layer_id = format!(
            "visual-layer-{}",
            sha256_bytes(
                json!([request.project_id, job_id, request.operation_id, "layer"])
                    .to_string()
                    .as_bytes()
            )
        );
        let reason = format!("Added visual {}", request.operation_id);
        let existing_job = self
            .store
            .get_job(job_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.job_not_found",
                    "The durable visual import job was not found",
                )
            })?;
        let existing_status = value_string(&existing_job, "status")?;
        let current = self.get_project(&request.project_id)?;
        let current_manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        current_manifest.validate_strict()?;
        let expected_next_revision = u64::try_from(request.expected_revision)
            .ok()
            .and_then(|revision| revision.checked_add(1));
        let committed = expected_next_revision.is_some_and(|revision| {
            current_manifest
                .revision_history
                .last()
                .is_some_and(|record| {
                    record.revision == revision
                        && record.actor == actor
                        && record.reason == reason
                        && record.changed_paths
                            == ["/visual_assets".to_string(), "/visual_layers".to_string()]
                })
                && current_manifest
                    .visual_assets
                    .iter()
                    .any(|asset| asset.id == asset_id)
                && current_manifest
                    .visual_layers
                    .iter()
                    .any(|layer| layer.id == layer_id && layer.asset_id == asset_id)
        });
        if committed {
            if existing_status != "completed" {
                let completed = self
                    .store
                    .complete_job(job_id)
                    .map_err(VideoServiceError::store)?;
                if !completed {
                    return Err(VideoServiceError::cancelled());
                }
            }
            return Ok(AddVisualAssetResult {
                project: current,
                asset_id,
                layer_id,
                job_id: job_id.to_string(),
                replayed: true,
            });
        }
        if !created && matches!(existing_status.as_str(), "preparing" | "running" | "queued") {
            return Err(VideoServiceError::new(
                "video.operation_in_progress",
                "This visual import is already running",
            )
            .retryable(true)
            .details(json!({ "job_id": job_id })));
        }
        if !created && matches!(existing_status.as_str(), "failed" | "cancelled") {
            self.store
                .resume_video_job(job_id, &["video_add_visual_asset"])
                .map_err(VideoServiceError::store)?;
        }
        if existing_status == "completed" {
            return Err(VideoServiceError::new(
                "video.integrity_failed",
                "The completed visual import has no matching project revision",
            ));
        }
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.to_string(), Arc::clone(&cancel));
        let _cancellation_registration = CancellationRegistration {
            cancellations: &self.cancellations,
            job_id: job_id.to_string(),
        };
        let expectation = ProjectExpectation {
            revision: request.expected_revision,
            version_id: request.expected_version_id.clone(),
        };
        ensure_project_matches(&current, &expectation)?;
        let resolved_source = self.claim_visual_source(request, job_id)?;
        let source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&resolved_source.path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.visual_not_found",
                    "The authorized visual could not be opened safely",
                    error,
                )
            })?;
        let inspection = resolved_source.inspection;
        let extension = visual_extension(inspection.mime_type);
        let visual_dir = self.project_dir(&request.project_id)?.join("visuals");
        self.secure_managed_directory(&visual_dir)?;
        let final_path = visual_dir.join(format!("{asset_id}.{extension}"));
        let staging_path =
            visual_dir.join(format!(".{asset_id}.{}.partial", Uuid::new_v4().simple()));
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:visual"),
            &self.video_root,
            with_disk_headroom(MAX_VISUAL_ASSET_BYTES, 1),
            "visual_import",
        )?;
        let staged_sha = copy_file_cancelable(
            source,
            resolved_source.expected_identity,
            &staging_path,
            cancel.as_ref(),
            |_| {},
        )?;
        let staged_inspection = inspect_image_file(&staging_path)?;
        if staged_sha != resolved_source.expected_sha256 || staged_inspection != inspection {
            let _ = fs::remove_file(&staging_path);
            return Err(VideoServiceError::new(
                "video.visual_changed",
                "The authorized visual changed while it was copied",
            ));
        }
        let newly_published = match fs::hard_link(&staging_path, &final_path) {
            Ok(()) => {
                fs::remove_file(&staging_path).map_err(|error| {
                    VideoServiceError::io(
                        "video.import_cleanup_failed",
                        "The visual staging link could not be finalized",
                        error,
                    )
                })?;
                secure_managed_file(&final_path)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let secure_result = secure_managed_file(&final_path);
                let _ = fs::remove_file(&staging_path);
                secure_result?;
                let existing_sha = sha256_file_with_cancel(&final_path, Some(cancel.as_ref()))?;
                if existing_sha != staged_sha || inspect_image_file(&final_path)? != inspection {
                    return Err(VideoServiceError::new(
                        "video.import_identity_conflict",
                        "This visual operation already owns different managed bytes",
                    ));
                }
                false
            }
            Err(error) => {
                let _ = fs::remove_file(&staging_path);
                return Err(VideoServiceError::io(
                    "video.publication_commit_failed",
                    "The visual could not be atomically published",
                    error,
                ));
            }
        };
        self.ensure_not_cancelled(cancel.as_ref())?;

        let lock = ProjectLock::acquire(self, &request.project_id, actor)?;
        let current = self.get_project(&request.project_id)?;
        if let Err(error) = ensure_project_matches(&current, &expectation) {
            if newly_published {
                let _ = fs::remove_file(&final_path);
            }
            return Err(error);
        }
        let created_at = utc_now();
        let relative_path = self.relative_managed_path(&final_path)?;
        let mut provenance = resolved_source.provenance;
        provenance.imported_at = created_at.clone();
        provenance.metadata.insert(
            "operation_id".to_string(),
            Value::String(request.operation_id.clone()),
        );
        let visual_asset = VisualAsset {
            id: asset_id.clone(),
            managed_path: relative_path,
            sha256: staged_sha.clone(),
            mime_type: inspection.mime_type,
            width: inspection.width,
            height: inspection.height,
            has_alpha: inspection.has_alpha,
            size_bytes: inspection.size_bytes,
            provenance: provenance.clone(),
            created_at: created_at.clone(),
        };
        let visual_layer = VisualLayer {
            id: layer_id.clone(),
            asset_id: asset_id.clone(),
            scene_id: request.scene_id.clone(),
            range: request.range,
            fit: request.fit,
            crop: request.crop,
            z_index: request.z_index,
            motion: request.motion.clone(),
            transition_in_us: request.transition_in_us,
            transition_out_us: request.transition_out_us,
        };
        visual_asset.validate()?;
        visual_layer.validate()?;
        if let Some(existing) = current
            .get("assets")
            .and_then(Value::as_array)
            .and_then(|assets| {
                assets.iter().find(|asset| {
                    asset.get("id").and_then(Value::as_str) == Some(asset_id.as_str())
                })
            })
        {
            if existing.get("content_sha256").and_then(Value::as_str) != Some(staged_sha.as_str())
                || existing.get("local_path").and_then(Value::as_str)
                    != Some(final_path.to_string_lossy().as_ref())
            {
                return Err(VideoServiceError::new(
                    "video.import_identity_conflict",
                    "The durable visual row owns different content",
                ));
            }
        } else {
            self.store
                .upsert_video_asset(&json!({
                    "id": asset_id,
                    "project_id": request.project_id,
                    "kind": "image",
                    "source_kind": if matches!(request.origin, VisualAssetOrigin::GeneratedLocally { .. }) { "generated" } else { "local" },
                    "local_path": final_path,
                    "mime_type": inspection.mime_type.as_mime(),
                    "content_sha256": staged_sha,
                    "size_bytes": inspection.size_bytes,
                    "status": "ready",
                    "probe": {
                        "width": inspection.width,
                        "height": inspection.height,
                        "has_alpha": inspection.has_alpha,
                    },
                    "provenance": provenance,
                }))
                .map_err(VideoServiceError::store)?;
        }
        self.ensure_not_cancelled(cancel.as_ref())?;
        let mut manifest: VideoProjectManifest = serde_json::from_value(
            current
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.visual_assets.push(visual_asset);
        manifest.visual_layers.push(visual_layer);
        let next_revision = manifest.revision.checked_add(1).ok_or_else(|| {
            VideoServiceError::new(
                "video.revision_overflow",
                "The visual revision could not be advanced",
            )
        })?;
        manifest.revision = next_revision;
        manifest.updated_at = created_at.clone();
        let changed_paths = vec!["/visual_assets".to_string(), "/visual_layers".to_string()];
        let invalidated_stages =
            invalidated_stages_for_manifest_changes(&changed_paths.iter().cloned().collect());
        manifest.revision_history.push(RevisionRecord {
            id: new_id(),
            revision: next_revision,
            parent_id: manifest
                .revision_history
                .last()
                .map(|record| record.id.clone()),
            actor: actor.to_string(),
            reason: reason.clone(),
            changed_paths,
            invalidated_stages,
            created_at,
        });
        manifest.validate_strict()?;
        let manifest_value = serde_json::to_value(&manifest).map_err(json_error)?;
        let project = self
            .store
            .commit_video_manifest_and_complete_job(
                &request.project_id,
                expectation.revision,
                &manifest_value,
                actor,
                &reason,
                &lock.token,
                current.get("status").and_then(Value::as_str),
                job_id,
            )
            .map_err(VideoServiceError::store)?;
        drop(lock);
        Ok(AddVisualAssetResult {
            project,
            asset_id,
            layer_id,
            job_id: job_id.to_string(),
            replayed: false,
        })
    }

    /// Download the poster frame for `canonical_url` into the managed video root.
    ///
    /// The intake dialog cannot fetch the remote thumbnail itself — the webview's content security
    /// policy permits no external origins — so the bytes are pulled here through the same protected
    /// proxy that reads the link metadata, and served back over the local media origin.
    fn cache_link_thumbnail(
        &self,
        canonical_url: &str,
        yt_dlp: &Path,
        proxy_url: &str,
    ) -> Option<PathBuf> {
        let directory = self.video_root.join("link-previews");
        fs::create_dir_all(&directory).ok()?;
        secure_private_directory(&directory).ok()?;
        let key = hex_digest(canonical_url.as_bytes());
        if let Some(existing) = find_cached_thumbnail(&directory, &key) {
            return Some(existing);
        }
        let template = directory.join(format!("{key}.%(ext)s"));
        let args = vec![
            OsString::from("--ignore-config"),
            OsString::from("--skip-download"),
            OsString::from("--write-thumbnail"),
            OsString::from("--no-playlist"),
            OsString::from("--no-warnings"),
            OsString::from("--socket-timeout"),
            OsString::from("10"),
            OsString::from("--proxy"),
            OsString::from(proxy_url),
            OsString::from("-o"),
            template.into_os_string(),
            OsString::from("--"),
            OsString::from(canonical_url),
        ];
        let captured = run_captured_command(
            yt_dlp,
            &args,
            LINK_THUMBNAIL_TIMEOUT,
            None,
            MAX_THUMBNAIL_CAPTURE_BYTES,
            Some(proxy_url),
            None,
        )
        .ok()?;
        if !captured.status.success() {
            return None;
        }
        find_cached_thumbnail(&directory, &key)
    }

    pub fn preview_link(&self, raw_url: &str) -> ServiceResult<LinkPreview> {
        let validated = validate_import_url(raw_url)?;
        if validated.is_playlist {
            return Err(VideoServiceError::new(
                "video.playlist_not_allowed",
                "Import one source URL at a time; playlists are not enabled",
            ));
        }
        let runtime = self.runtime_status(false);
        let yt_dlp = required_tool_path(
            &runtime.yt_dlp,
            "video.yt_dlp_unavailable",
            "yt-dlp is required to preview this link",
        )?;
        preflight_import_url_destination(&validated)?;
        let proxy = PublicHttpsProxy::start()?;
        let args = vec![
            OsString::from("--ignore-config"),
            OsString::from("--dump-single-json"),
            OsString::from("--skip-download"),
            OsString::from("--no-playlist"),
            OsString::from("--no-warnings"),
            OsString::from("--socket-timeout"),
            OsString::from("15"),
            OsString::from("--match-filter"),
            OsString::from("!is_live & duration <= 21600"),
            OsString::from("--proxy"),
            OsString::from(proxy.url()),
            OsString::from("--"),
            OsString::from(&validated.canonical),
        ];
        let captured = run_captured_command(
            yt_dlp,
            &args,
            LINK_PREVIEW_TIMEOUT,
            None,
            MAX_CAPTURE_BYTES,
            Some(proxy.url()),
            None,
        )?;
        if !captured.status.success() {
            return Err(command_failed_error(
                "video.link_preview_failed",
                "The link metadata could not be read",
                &captured,
                true,
            ));
        }
        let metadata: Value = serde_json::from_slice(&captured.stdout).map_err(|error| {
            VideoServiceError::new(
                "video.link_preview_invalid",
                "yt-dlp returned invalid metadata",
            )
            .details(json!({ "diagnostic": error.to_string() }))
        })?;
        if metadata.get("_type").and_then(Value::as_str) == Some("playlist")
            || metadata.get("entries").is_some_and(Value::is_array)
        {
            return Err(VideoServiceError::new(
                "video.playlist_not_allowed",
                "Import one source URL at a time; playlists are not enabled",
            ));
        }
        let title = metadata
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Untitled source")
            .to_string();
        let duration_us = metadata
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|seconds| (seconds * 1_000_000.0).round() as i64);
        let formats = metadata.get("formats").and_then(Value::as_array);
        let has_video = formats.is_some_and(|items| {
            items.iter().any(|format| {
                format
                    .get("vcodec")
                    .and_then(Value::as_str)
                    .is_some_and(|codec| codec != "none")
            })
        }) || metadata
            .get("vcodec")
            .and_then(Value::as_str)
            .is_some_and(|codec| codec != "none");
        let has_audio = formats.is_some_and(|items| {
            items.iter().any(|format| {
                format
                    .get("acodec")
                    .and_then(Value::as_str)
                    .is_some_and(|codec| codec != "none")
            })
        }) || metadata
            .get("acodec")
            .and_then(Value::as_str)
            .is_some_and(|codec| codec != "none");
        let thumbnail_url = metadata
            .get("thumbnail")
            .and_then(Value::as_str)
            .filter(|url| url.starts_with("https://"))
            .map(str::to_string);
        // Best effort: a missing poster must never fail the preview the user is waiting on.
        let thumbnail_path = thumbnail_url.as_ref().and_then(|_| {
            self.cache_link_thumbnail(&validated.canonical, yt_dlp, proxy.url())
                .map(|path| path.to_string_lossy().to_string())
        });
        Ok(LinkPreview {
            canonical_url: validated.canonical,
            provider: format!("{:?}", validated.provider).to_ascii_lowercase(),
            source_id: validated.source_id,
            title,
            creator: metadata
                .get("uploader")
                .or_else(|| metadata.get("channel"))
                .and_then(Value::as_str)
                .map(str::to_string),
            duration_us,
            thumbnail_url,
            thumbnail_path,
            view_count: metadata.get("view_count").and_then(Value::as_u64),
            upload_date: metadata
                .get("upload_date")
                .and_then(Value::as_str)
                .filter(|value| value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()))
                .map(str::to_string),
            extractor: metadata
                .get("extractor_key")
                .or_else(|| metadata.get("extractor"))
                .and_then(Value::as_str)
                .map(str::to_string),
            has_video,
            has_audio,
            rights_confirmation_required: validated.rights_confirmation_required,
        })
    }

    pub fn queue_local_import(
        self: &Arc<Self>,
        request: LocalImportRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        self.queue_local_import_inner(request, None, callback)
    }

    /// Parent-bound form used by durable prompt/audio-to-video workflows. A
    /// retry after queueing but before the parent checkpoint adopts the exact
    /// same import job instead of copying/importing the History artifact twice.
    pub fn queue_local_import_idempotent(
        self: &Arc<Self>,
        request: LocalImportRequest,
        parent_job_id: &str,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        validate_safe_component(parent_job_id, "video.invalid_parent_job_id")?;
        let parent = self
            .store
            .get_job(parent_job_id)
            .map_err(VideoServiceError::store)?
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.parent_job_not_found",
                    "The prompt-to-video parent task was not found",
                )
            })?;
        match parent.get("status").and_then(Value::as_str) {
            Some("queued" | "preparing" | "running") => {}
            Some("cancelled") => return Err(VideoServiceError::cancelled()),
            _ => {
                return Err(VideoServiceError::new(
                    "video.parent_job_inactive",
                    "The prompt-to-video parent task is no longer active",
                ))
            }
        }
        self.queue_local_import_inner(request, Some(parent_job_id.to_string()), callback)
    }

    fn queue_local_import_inner(
        self: &Arc<Self>,
        request: LocalImportRequest,
        parent_job_id: Option<String>,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        self.get_project(&request.project_id)?;
        let source = fs::canonicalize(&request.source_path).map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected local media could not be opened",
                error,
            )
        })?;
        let metadata = fs::symlink_metadata(&source).map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected local media could not be inspected",
                error,
            )
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.invalid_source",
                "The selected media must resolve to a regular local file",
            ));
        }
        validate_source_size(metadata.len())?;
        ensure_disk_capacity(
            &self.video_root,
            with_disk_headroom(metadata.len(), 3),
            "local_import",
        )?;
        let origin = self.detect_local_import_origin(&source)?;
        let durable_request = DurableLocalImportRequest {
            project_id: request.project_id.clone(),
            source_path: source,
            actor: request.actor.clone(),
            title: request.title.clone(),
            origin,
            parent_job_id: parent_job_id.clone(),
            priority: default_normal_priority(),
        };
        let durable_value = serde_json::to_value(&durable_request).map_err(json_error)?;
        let (job_id, created) = if let Some(parent_job_id) = parent_job_id.as_deref() {
            self.store
                .create_idempotent_job(
                    "video_import_local",
                    &format!("video-prompt-import:{parent_job_id}"),
                    &durable_value,
                )
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.idempotency_conflict",
                        "The prompt parent is already bound to a different local import request",
                    )
                })?
        } else {
            (
                self.store
                    .create_job("video_import_local", &durable_value)
                    .map_err(VideoServiceError::store)?,
                true,
            )
        };
        let project_id = durable_request.project_id.clone();
        if !created {
            let job = self
                .store
                .get_job(&job_id)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.job_not_found",
                        "The durable prompt import could not be reloaded",
                    )
                })?;
            return match job.get("status").and_then(Value::as_str) {
                Some("failed" | "cancelled") => self.resume_job(&job_id, callback),
                Some("queued" | "preparing" | "running" | "completed") => Ok(QueuedVideoJob {
                    job_id,
                    project_id,
                    kind: "video_import_local".to_string(),
                }),
                _ => Err(VideoServiceError::new(
                    "video.job_state_invalid",
                    "The durable prompt import has an unsupported state",
                )),
            };
        }
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_local_import(&job_id, &durable_request, &cancel, callback.as_ref())
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: "video_import_local".to_string(),
        })
    }

    fn detect_local_import_origin(
        &self,
        source_path: &Path,
    ) -> ServiceResult<DurableLocalImportOrigin> {
        let raw_path = source_path.to_str().ok_or_else(|| {
            VideoServiceError::new(
                "video.invalid_source_path",
                "The selected source path is not valid UTF-8",
            )
        })?;
        let history = match self.store.get_registered_history_by_audio_path(raw_path) {
            Ok(history) => history,
            // The Store intentionally rejects arbitrary paths before querying
            // History. That result means this is an ordinary user-selected
            // upload, not a failed soundAr integrity check.
            Err(error) if error.contains("outside the managed artifact directory") => None,
            Err(error) => return Err(VideoServiceError::store(error)),
        };
        let Some(history) = history else {
            return Ok(DurableLocalImportOrigin::UserUpload);
        };
        let generation_kind = value_string(&history, "generation_kind")?;
        if !matches!(generation_kind.as_str(), "speech" | "music") {
            return Err(VideoServiceError::new(
                "video.invalid_soundar_origin",
                "The registered soundAr artifact has an unsupported generation kind",
            ));
        }
        Ok(DurableLocalImportOrigin::SoundArHistory {
            history_id: value_string(&history, "id")?,
            generation_job_id: value_string(&history, "job_id")?,
            generation_kind,
            model_id: value_string(&history, "model_id")?,
            voice: history
                .get("voice")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            engine: value_string(&history, "engine")?,
        })
    }

    fn verify_local_import_origin(
        &self,
        source_path: &Path,
        origin: &DurableLocalImportOrigin,
    ) -> ServiceResult<()> {
        if matches!(origin, DurableLocalImportOrigin::UserUpload) {
            return Ok(());
        }
        let current = self.detect_local_import_origin(source_path)?;
        if &current != origin {
            return Err(VideoServiceError::new(
                "video.soundar_origin_changed",
                "The registered soundAr artifact no longer matches the durable import request",
            )
            .details(json!({ "expected": origin, "actual": current })));
        }
        Ok(())
    }

    pub fn queue_link_import(
        self: &Arc<Self>,
        request: LinkImportRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        self.get_project(&request.project_id)?;
        let validated = validate_import_url(&request.url)?;
        if validated.is_playlist {
            return Err(VideoServiceError::new(
                "video.playlist_not_allowed",
                "Import one source URL at a time; playlists are not enabled",
            ));
        }
        if request.rights.confirmed_url != validated.canonical {
            return Err(VideoServiceError::new(
                "video.rights_url_mismatch",
                "Confirm rights for the exact canonical URL shown in the intake dialog",
            )
            .details(json!({ "expected_url": validated.canonical })));
        }
        require_text(
            &request.rights.statement,
            "video.rights_required",
            "A rights statement is required",
        )?;
        require_text(
            &request.rights.confirmed_by,
            "video.rights_required",
            "The confirming user is required",
        )?;
        let runtime = self.runtime_status(false);
        if !runtime.ready_for_link_import {
            return Err(VideoServiceError::new(
                "video.link_runtime_unavailable",
                "Link import requires yt-dlp, FFmpeg, FFprobe and a supported JavaScript runtime",
            )
            .details(json!({ "setup_actions": runtime.setup_actions })));
        }
        preflight_import_url_destination(&validated)?;
        ensure_disk_capacity(
            &self.video_root,
            with_disk_headroom(MAX_SOURCE_BYTES, 3),
            "link_import",
        )?;
        let receipt = self
            .store
            .record_video_rights_receipt(
                Some(&request.project_id),
                &validated.canonical,
                &request.rights.statement,
                &request.rights.confirmed_by,
            )
            .map_err(VideoServiceError::store)?;
        let typed_rights = RightsConfirmation {
            id: value_string(&receipt, "id")?,
            source_uri: validated.canonical.clone(),
            source_uri_sha256: value_string(&receipt, "url_sha256")?,
            basis: request.rights.basis.clone(),
            confirmation_text: request.rights.statement.trim().to_string(),
            confirmed_by: request.rights.confirmed_by.trim().to_string(),
            confirmed_at: normalize_utc_timestamp(&value_string(&receipt, "confirmed_at")?)?,
            single_source_only: true,
        };
        typed_rights.validate()?;
        let durable_request = serde_json::to_value(DurableLinkImportRequest {
            request: request.clone(),
            canonical_url: validated.canonical.clone(),
            rights_confirmation: typed_rights.clone(),
        })
        .map_err(json_error)?;
        let job_id = self
            .store
            .create_job("video_import_link", &durable_request)
            .map_err(VideoServiceError::store)?;
        let project_id = request.project_id.clone();
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_link_import(
                    &job_id,
                    &request,
                    &validated.canonical,
                    typed_rights,
                    &cancel,
                    callback.as_ref(),
                )
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: "video_import_link".to_string(),
        })
    }

    pub fn queue_portrait_render(
        self: &Arc<Self>,
        mut request: PortraitRenderRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        let project = self.get_project(&request.project_id)?;
        let expectation = expectation_from_optional(
            &project,
            request.expected_revision,
            request.expected_version_id.as_deref(),
        )?;
        request.expected_revision = Some(expectation.revision);
        request.expected_version_id = Some(expectation.version_id);
        let durable_request = serde_json::to_value(&request).map_err(json_error)?;
        let kind = match request.profile {
            RenderProfile::Final => "video_render_final",
            RenderProfile::Proxy | RenderProfile::Preview => "video_render_preview",
        };
        let job_id = self
            .store
            .create_job(kind, &durable_request)
            .map_err(VideoServiceError::store)?;
        let project_id = request.project_id.clone();
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_portrait_render(&job_id, &request, &cancel, callback.as_ref())
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: kind.to_string(),
        })
    }

    /// Queues the canonical reviewed-scene timeline renderer. The required
    /// revision and version pair binds every cache lookup and publication to
    /// the exact edit the caller reviewed.
    pub fn queue_timeline_render(
        self: &Arc<Self>,
        request: TimelineRenderRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        require_text(
            &request.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        let project = self.get_project(&request.project_id)?;
        ensure_project_matches(
            &project,
            &ProjectExpectation {
                revision: request.expected_revision,
                version_id: request.expected_version_id.clone(),
            },
        )?;
        let durable_request = serde_json::to_value(&request).map_err(json_error)?;
        let kind = match request.profile {
            TimelineRenderProfile::Preview => "video_render_timeline_preview",
            TimelineRenderProfile::Final => "video_render_timeline_final",
        };
        let job_id = self
            .store
            .create_job(kind, &durable_request)
            .map_err(VideoServiceError::store)?;
        let project_id = request.project_id.clone();
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_timeline_render(&job_id, &request, &cancel, callback.as_ref())
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: kind.to_string(),
        })
    }

    pub fn queue_timeline_render_batch(
        self: &Arc<Self>,
        request: TimelineRenderBatchRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        require_text(
            &request.base.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        if request.base.variation != 0 {
            return Err(VideoServiceError::new(
                "video.invalid_variation_batch",
                "The batch base variation must be zero; list every requested variation explicitly",
            ));
        }
        if request.variations.is_empty() || request.variations.len() > 8 {
            return Err(VideoServiceError::new(
                "video.invalid_variation_batch",
                "Choose between 1 and 8 render variations",
            ));
        }
        let unique = request.variations.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != request.variations.len() {
            return Err(VideoServiceError::new(
                "video.invalid_variation_batch",
                "Each render variation discriminator must be unique",
            ));
        }
        ensure_project_matches(
            &self.get_project(&request.base.project_id)?,
            &ProjectExpectation {
                revision: request.base.expected_revision,
                version_id: request.base.expected_version_id.clone(),
            },
        )?;
        let durable_request = serde_json::to_value(&request).map_err(json_error)?;
        let kind = match request.base.profile {
            TimelineRenderProfile::Preview => "video_render_timeline_batch_preview",
            TimelineRenderProfile::Final => "video_render_timeline_batch_final",
        };
        let job_id = self
            .store
            .create_job(kind, &durable_request)
            .map_err(VideoServiceError::store)?;
        let project_id = request.base.project_id.clone();
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_timeline_render_batch(&job_id, &request, &cancel, callback.as_ref())
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: kind.to_string(),
        })
    }

    /// Adopts already-generated, registered soundAr History speech and swaps
    /// only the exact narration clips it targets. RuntimeState owns synthesis;
    /// this service owns media conformance, provenance, timeline CAS, and the
    /// durable revision job shared by native and Codex surfaces.
    pub fn queue_narration_replacement(
        self: &Arc<Self>,
        request: ReplaceNarrationRequest,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        require_text(
            &request.actor,
            "video.invalid_actor",
            "An actor is required",
        )?;
        if request.replacements.is_empty() || request.replacements.len() > 32 {
            return Err(VideoServiceError::new(
                "video.invalid_narration_replacement",
                "Choose between 1 and 32 narration replacements",
            ));
        }
        if let Some(parent_job_id) = request.parent_job_id.as_deref() {
            validate_safe_component(parent_job_id, "video.invalid_parent_job_id")?;
        }
        let mut targets = BTreeSet::new();
        for replacement in &request.replacements {
            for value in [
                replacement.history_id.as_str(),
                replacement.voice_id.as_str(),
                replacement.model_id.as_str(),
                replacement.speaker.as_str(),
                replacement.language.as_str(),
            ] {
                require_text(
                    value,
                    "video.invalid_narration_replacement",
                    "Narration replacement route fields are required",
                )?;
            }
            let target = replacement
                .binding_id
                .as_deref()
                .map(|value| format!("binding:{value}"))
                .or_else(|| {
                    replacement
                        .clip_id
                        .as_deref()
                        .map(|value| format!("clip:{value}"))
                })
                .or_else(|| {
                    replacement
                        .scene_id
                        .as_deref()
                        .map(|value| format!("scene:{value}"))
                })
                // A dialogue turn is a narration target in its own right: a line has a take
                // whether or not it has been placed on the timeline as a clip yet.
                .or_else(|| {
                    replacement
                        .turn_id
                        .as_deref()
                        .map(|value| format!("turn:{value}"))
                })
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.narration_target_required",
                        "Select an exact narration binding, clip, scene, or dialogue turn",
                    )
                })?;
            if !targets.insert(target) {
                return Err(VideoServiceError::new(
                    "video.duplicate_narration_target",
                    "A narration target may be replaced only once per revision",
                ));
            }
        }
        let expectation = ProjectExpectation {
            revision: request.expected_revision,
            version_id: request.expected_version_id.clone(),
        };
        let durable_request = serde_json::to_value(&request).map_err(json_error)?;
        let (job_id, created) = if let Some(parent_job_id) = request.parent_job_id.as_deref() {
            self.store
                .create_idempotent_job(
                    "video_replace_narration",
                    &format!("narration-replacement:{parent_job_id}"),
                    &durable_request,
                )
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.idempotency_conflict",
                        "The narration parent job is already bound to a different replacement request",
                    )
                })?
        } else {
            (
                self.store
                    .create_job("video_replace_narration", &durable_request)
                    .map_err(VideoServiceError::store)?,
                true,
            )
        };
        let project_id = request.project_id.clone();
        if !created {
            let job = self
                .store
                .get_job(&job_id)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.job_not_found",
                        "The durable narration replacement could not be reloaded",
                    )
                })?;
            return match job.get("status").and_then(Value::as_str) {
                Some("failed" | "cancelled") => self.resume_job(&job_id, callback),
                Some("preparing" | "queued" | "running" | "completed") => Ok(QueuedVideoJob {
                    job_id,
                    project_id,
                    kind: "video_replace_narration".to_string(),
                }),
                _ => Err(VideoServiceError::new(
                    "video.job_state_invalid",
                    "The durable narration replacement has an unsupported state",
                )),
            };
        }
        if let Err(error) = ensure_project_matches(&self.get_project(&project_id)?, &expectation) {
            let _ = self.store.fail_job(&job_id, &error.stable_message());
            return Err(error);
        }
        self.spawn_worker(
            job_id.clone(),
            project_id.clone(),
            callback,
            move |service, job_id, cancel, callback| {
                service.perform_narration_replacement(&job_id, &request, &cancel, callback.as_ref())
            },
        )?;
        Ok(QueuedVideoJob {
            job_id,
            project_id,
            kind: "video_replace_narration".to_string(),
        })
    }

    /// Resumes a failed/cancelled durable Video Studio job through its owning
    /// runner. This deliberately reuses the original job id and request; it
    /// never creates a shadow workflow or duplicate job.
    pub fn resume_job(
        self: &Arc<Self>,
        job_id: &str,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<QueuedVideoJob> {
        let allowed = [
            "video_import_local",
            "video_import_link",
            "video_render_preview",
            "video_render_final",
            "video_render_timeline_preview",
            "video_render_timeline_final",
            "video_render_timeline_batch_preview",
            "video_render_timeline_batch_final",
            "video_replace_narration",
            "video_publish_package",
        ];
        let (job, durable_request) =
            self.store
                .resume_video_job(job_id, &allowed)
                .map_err(|error| {
                    VideoServiceError::new(
                        "video.resume_rejected",
                        "The selected video task cannot be resumed",
                    )
                    .details(json!({ "diagnostic": error }))
                })?;
        let kind = value_string(&job, "kind")?;
        let dispatch =
            self.dispatch_resumed_job(job_id.to_string(), &kind, durable_request, callback);
        match dispatch {
            Ok(project_id) => Ok(QueuedVideoJob {
                job_id: job_id.to_string(),
                project_id,
                kind,
            }),
            Err(error) => {
                let persistence = self.store.fail_job(job_id, &error.stable_message());
                match persistence {
                    Ok(()) => Err(error),
                    Err(store_error) => Err(VideoServiceError::new(
                        "video.job_state_failed",
                        "The task could not be resumed and that durable failure could not be saved",
                    )
                    .retryable(true)
                    .details(json!({
                        "resume_error": error,
                        "store_error": store_error,
                    }))),
                }
            }
        }
    }

    fn dispatch_resumed_job(
        self: &Arc<Self>,
        job_id: String,
        kind: &str,
        durable_request: Value,
        callback: Option<ProgressCallback>,
    ) -> ServiceResult<String> {
        match kind {
            "video_import_local" => {
                let mut request: DurableLocalImportRequest = parse_resume_request(durable_request)?;
                let source = fs::canonicalize(&request.source_path).map_err(|error| {
                    VideoServiceError::io(
                        "video.source_not_found",
                        "The original local media is no longer available",
                        error,
                    )
                })?;
                let metadata = fs::symlink_metadata(&source).map_err(|error| {
                    VideoServiceError::io(
                        "video.source_not_found",
                        "The original local media could not be inspected",
                        error,
                    )
                })?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(VideoServiceError::new(
                        "video.invalid_source",
                        "The original local source no longer resolves to a regular file",
                    ));
                }
                request.source_path = source;
                let project_id = request.project_id.clone();
                self.get_project(&project_id)?;
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_local_import(&job_id, &request, &cancel, callback.as_ref())
                    },
                )?;
                Ok(project_id)
            }
            "video_import_link" => {
                let durable: DurableLinkImportRequest = parse_resume_request(durable_request)?;
                durable.rights_confirmation.validate()?;
                let validated = validate_import_url(&durable.request.url)?;
                if validated.is_playlist
                    || validated.canonical != durable.canonical_url
                    || durable.request.rights.confirmed_url != durable.canonical_url
                    || durable.rights_confirmation.source_uri != durable.canonical_url
                {
                    return Err(VideoServiceError::new(
                        "video.resume_request_invalid",
                        "The durable link task is not bound to one exact authorized URL",
                    ));
                }
                let project_id = durable.request.project_id.clone();
                self.get_project(&project_id)?;
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_link_import(
                            &job_id,
                            &durable.request,
                            &durable.canonical_url,
                            durable.rights_confirmation,
                            &cancel,
                            callback.as_ref(),
                        )
                    },
                )?;
                Ok(project_id)
            }
            "video_render_preview" | "video_render_final" => {
                let request: PortraitRenderRequest = parse_resume_request(durable_request)?;
                let expected_kind = if request.profile == RenderProfile::Final {
                    "video_render_final"
                } else {
                    "video_render_preview"
                };
                if kind != expected_kind {
                    return Err(VideoServiceError::new(
                        "video.resume_request_invalid",
                        "The saved render profile does not match its durable workflow kind",
                    ));
                }
                let project_id = request.project_id.clone();
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_portrait_render(
                            &job_id,
                            &request,
                            &cancel,
                            callback.as_ref(),
                        )
                    },
                )?;
                Ok(project_id)
            }
            "video_render_timeline_preview" | "video_render_timeline_final" => {
                let request: TimelineRenderRequest = parse_resume_request(durable_request)?;
                let expected_kind = match request.profile {
                    TimelineRenderProfile::Preview => "video_render_timeline_preview",
                    TimelineRenderProfile::Final => "video_render_timeline_final",
                };
                if kind != expected_kind {
                    return Err(VideoServiceError::new(
                        "video.resume_request_invalid",
                        "The saved timeline profile does not match its durable workflow kind",
                    ));
                }
                let project_id = request.project_id.clone();
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_timeline_render(
                            &job_id,
                            &request,
                            &cancel,
                            callback.as_ref(),
                        )
                    },
                )?;
                Ok(project_id)
            }
            "video_render_timeline_batch_preview" | "video_render_timeline_batch_final" => {
                let request: TimelineRenderBatchRequest = parse_resume_request(durable_request)?;
                let expected_kind = match request.base.profile {
                    TimelineRenderProfile::Preview => "video_render_timeline_batch_preview",
                    TimelineRenderProfile::Final => "video_render_timeline_batch_final",
                };
                if kind != expected_kind {
                    return Err(VideoServiceError::new(
                        "video.resume_request_invalid",
                        "The saved timeline batch profile does not match its durable workflow kind",
                    ));
                }
                let project_id = request.base.project_id.clone();
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_timeline_render_batch(
                            &job_id,
                            &request,
                            &cancel,
                            callback.as_ref(),
                        )
                    },
                )?;
                Ok(project_id)
            }
            "video_replace_narration" => {
                let request: ReplaceNarrationRequest = parse_resume_request(durable_request)?;
                let project_id = request.project_id.clone();
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, callback| {
                        service.perform_narration_replacement(
                            &job_id,
                            &request,
                            &cancel,
                            callback.as_ref(),
                        )
                    },
                )?;
                Ok(project_id)
            }
            "video_publish_package" => {
                let request: PublishPackageRequest = parse_resume_request(durable_request)?;
                let project_id = request.project_id.clone();
                self.spawn_worker(
                    job_id,
                    project_id.clone(),
                    callback,
                    move |service, job_id, cancel, _callback| {
                        service.ensure_not_cancelled(&cancel)?;
                        let status = service
                            .store
                            .start_job(&job_id)
                            .map_err(VideoServiceError::store)?;
                        if status == "cancelled" {
                            return Err(VideoServiceError::cancelled());
                        }
                        let project = service.get_project(&request.project_id)?;
                        service
                            .perform_publish_package(&job_id, &request, project, &cancel)
                            .map(|_| ())
                    },
                )?;
                Ok(project_id)
            }
            _ => Err(VideoServiceError::new(
                "video.resume_unsupported",
                "This Video Studio workflow has no safe resume runner",
            )),
        }
    }

    pub fn cancel_job(&self, job_id: &str) -> ServiceResult<bool> {
        if let Some(flag) = self
            .cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(job_id)
            .cloned()
        {
            flag.store(true, Ordering::Release);
        }
        self.store
            .cancel_job(job_id)
            .map_err(VideoServiceError::store)
    }

    pub fn wait_for_job(
        &self,
        job_id: &str,
        project_id: &str,
        timeout: Duration,
    ) -> ServiceResult<VideoJobResult> {
        let deadline = Instant::now() + timeout;
        loop {
            let job = self
                .store
                .get_job(job_id)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new("video.job_not_found", "The video job was not found")
                })?;
            match job.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    return Ok(VideoJobResult {
                        job_id: job_id.to_string(),
                        project_id: project_id.to_string(),
                        job,
                        project: self.get_project(project_id)?,
                    });
                }
                Some("failed") => {
                    return Err(VideoServiceError::new(
                        "video.job_failed",
                        job.get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("The video job failed"),
                    ));
                }
                Some("cancelled") => return Err(VideoServiceError::cancelled()),
                _ if Instant::now() >= deadline => {
                    return Err(VideoServiceError::new(
                        "video.job_wait_timeout",
                        "The video job is still running",
                    )
                    .retryable(true)
                    .details(json!({ "job": job })));
                }
                _ => thread::sleep(Duration::from_millis(75)),
            }
        }
    }

    fn spawn_worker<F>(
        self: &Arc<Self>,
        job_id: String,
        project_id: String,
        callback: Option<ProgressCallback>,
        work: F,
    ) -> ServiceResult<()>
    where
        F: FnOnce(
                Arc<VideoStudioService>,
                String,
                Arc<AtomicBool>,
                Option<ProgressCallback>,
            ) -> ServiceResult<()>
            + Send
            + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.clone(), Arc::clone(&cancel));
        let service = Arc::clone(self);
        let thread_job_id = job_id.clone();
        let builder =
            thread::Builder::new().name(format!("video-{}", &job_id[..job_id.len().min(12)]));
        if let Err(error) = builder.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(
                    Arc::clone(&service),
                    thread_job_id.clone(),
                    Arc::clone(&cancel),
                    callback.clone(),
                )
            }));
            let result = match result {
                Ok(result) => result,
                Err(_) => Err(VideoServiceError::new(
                    "video.worker_panicked",
                    "The local video worker stopped unexpectedly",
                )),
            };
            match result {
                Ok(()) => {
                    if cancel.load(Ordering::Acquire) {
                        if let Err(store_error) = service.store.cancel_job(&thread_job_id) {
                            emit_job_state_error(
                                callback.as_ref(),
                                &thread_job_id,
                                &project_id,
                                "cancelled",
                                store_error,
                            );
                        }
                    } else {
                        match service.store.complete_job(&thread_job_id) {
                            Ok(true) => emit_progress(
                                callback.as_ref(),
                                &thread_job_id,
                                &project_id,
                                "completed",
                                1.0,
                                "Video task completed",
                                None,
                                None,
                            ),
                            // A concurrent cancellation or terminal transition won
                            // the durable CAS. Never announce completion afterward.
                            Ok(false) => {}
                            Err(store_error) => emit_job_state_error(
                                callback.as_ref(),
                                &thread_job_id,
                                &project_id,
                                "completed",
                                store_error,
                            ),
                        }
                    }
                }
                Err(error) if error.code == "video.cancelled" => {
                    if let Err(store_error) = service.store.cancel_job(&thread_job_id) {
                        emit_job_state_error(
                            callback.as_ref(),
                            &thread_job_id,
                            &project_id,
                            "cancelled",
                            store_error,
                        );
                    }
                }
                Err(error) => {
                    let persistence = service
                        .store
                        .fail_job(&thread_job_id, &error.stable_message());
                    match persistence {
                        Ok(()) => emit_progress(
                            callback.as_ref(),
                            &thread_job_id,
                            &project_id,
                            "failed",
                            1.0,
                            &error.message,
                            None,
                            Some(json!({ "error": error })),
                        ),
                        Err(store_error) => emit_job_state_error(
                            callback.as_ref(),
                            &thread_job_id,
                            &project_id,
                            "failed",
                            store_error,
                        ),
                    }
                }
            }
            service
                .cancellations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&thread_job_id);
        }) {
            self.cancellations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&job_id);
            let service_error = VideoServiceError::io(
                "video.worker_unavailable",
                "The local video worker could not be started",
                error,
            );
            if let Err(store_error) = self
                .store
                .fail_job(&job_id, &service_error.stable_message())
            {
                return Err(VideoServiceError::new(
                    "video.job_state_failed",
                    "The worker could not start and its durable failure state could not be saved",
                )
                .retryable(true)
                .details(json!({
                    "worker_error": service_error,
                    "store_error": store_error,
                })));
            }
            return Err(service_error);
        }
        Ok(())
    }
}

impl VideoStudioService {
    fn perform_portrait_render(
        &self,
        job_id: &str,
        request: &PortraitRenderRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        self.ensure_not_cancelled(cancel)?;
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let started = Instant::now();
        let project = self.get_project(&request.project_id)?;
        let expectation = declared_expectation(
            &project,
            request.expected_revision,
            request.expected_version_id.as_deref(),
        )?;
        let stage_key = match request.profile {
            RenderProfile::Final => "final_render",
            RenderProfile::Proxy | RenderProfile::Preview => "preview_render",
        };
        let reason = if request.profile == RenderProfile::Final {
            "Published a final portrait master"
        } else {
            "Published a fast portrait preview"
        };
        let invalidated_stage = if request.profile == RenderProfile::Final {
            RevisionStage::FinalRender
        } else {
            RevisionStage::Preview
        };
        let output_kind = if request.variation > 0 {
            "variation"
        } else if request.profile == RenderProfile::Final {
            "master"
        } else {
            "preview"
        };
        let semantic_role = format!("portrait-{output_kind}");
        let request_sha256 = sha256_bytes(&serde_json::to_vec(request).map_err(json_error)?);
        if let Err(conflict) = ensure_project_matches(&project, &expectation) {
            if let Some(output) = self.recover_single_render_output(
                &project,
                job_id,
                &expectation,
                &request.actor,
                reason,
                invalidated_stage,
                &request_sha256,
                &semantic_role,
                request.variation,
                output_kind,
                request.profile == RenderProfile::Final && request.variation == 0,
                "cache_key",
                None,
                cancel,
            )? {
                let current = project_expectation(&project)?;
                let provenance = output
                    .get("provenance")
                    .ok_or_else(|| invalid_store_shape("outputs.provenance"))?;
                let cache_key = provenance
                    .get("cache_key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_store_shape("outputs.provenance.cache_key"))?;
                let output_sha256 = value_string(&output, "sha256")?;
                self.checkpoint_stage(
                    &request.project_id,
                    Some(&current.version_id),
                    stage_key,
                    &format!("variation-{}", request.variation),
                    job_id,
                    "completed",
                    "light",
                    1.0,
                    cache_key,
                    Some(&output_sha256),
                    json!({ "output_id": output.get("id"), "idempotent_replay": true }),
                    None,
                )?;
                self.store
                    .update_job(job_id, "running", 0.99)
                    .map_err(VideoServiceError::store)?;
                emit_progress(
                    callback,
                    job_id,
                    &request.project_id,
                    "published",
                    0.99,
                    "The atomically published portrait render was recovered after restart",
                    Some(output),
                    Some(json!({ "idempotent_replay": true })),
                );
                return Ok(());
            }
            return Err(conflict);
        }
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        let source = select_manifest_source(&manifest, request.source_asset_id.as_deref())?;
        let source_path = self.resolve_managed_path(&source.managed_path)?;
        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to render video",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate rendered video",
        )?;
        let probe = probe_media(&source_path, ffprobe)?;
        let has_video = probe.primary_video_stream.is_some();
        let has_audio = probe.primary_audio_stream.is_some();
        if !has_video && !has_audio {
            return Err(VideoServiceError::new(
                "video.invalid_source",
                "The selected source is not playable",
            ));
        }
        validate_media_duration(probe.duration_us)?;
        let resources = render_resource_request(request.profile, runtime.h264_nvenc_runtime);
        let _lease =
            self.acquire_resources(job_id, &request.project_id, resources, cancel, callback)?;
        let stage = match request.profile {
            RenderProfile::Final => CacheStage::FinalRender,
            RenderProfile::Proxy | RenderProfile::Preview => CacheStage::PreviewRender,
        };
        let profile_value = json!({
            "profile": request.profile,
            "layout": request.layout,
            "variation": request.variation,
            "audio_only": !has_video,
            "title": request.title,
        });
        let cache_key = CacheKeyBuilder::new(stage, SERVICE_VERSION)
            .artifact("source", source.sha256.clone())
            .tool_version(
                "ffmpeg",
                runtime
                    .ffmpeg
                    .version
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            )
            .manifest_slice(json!({
                "source_asset_id": source.id,
                "source_range": [0, source.probe.duration_us.0],
                "captions": manifest.captions,
                "layout": manifest.layout,
                "audio_mix": manifest.audio_mix,
            }))
            .profile(profile_value.clone())
            .build()?
            .into_string();
        self.checkpoint_stage(
            &request.project_id,
            project
                .get("version")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str),
            stage_key,
            &format!("variation-{}", request.variation),
            job_id,
            "running",
            resource_class_name(resources.class),
            0.03,
            &cache_key,
            None,
            json!({ "profile": request.profile, "layout": request.layout }),
            None,
        )?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "rendering",
            0.05,
            if request.profile == RenderProfile::Final {
                "Rendering final portrait master"
            } else {
                "Rendering fast portrait preview"
            },
            None,
            None,
        );

        let cache_namespace = if request.profile == RenderProfile::Final {
            "final_render"
        } else {
            "preview_render"
        };
        let mut cache_hit = false;
        let output_path = if let Some(cache) = self
            .store
            .get_video_cache(&cache_key)
            .map_err(VideoServiceError::store)?
        {
            cache_hit = true;
            PathBuf::from(value_string(&cache, "artifact_path")?)
        } else {
            let output = self.cache_path(cache_namespace, &cache_key, "mp4")?;
            if !output.is_file() {
                let _storage_lease = self.reserve_storage(
                    format!("{job_id}:portrait:{cache_key}"),
                    &self.video_root,
                    with_disk_headroom(
                        estimated_render_bytes(probe.duration_us, request.profile)?,
                        2,
                    ),
                    "portrait_render",
                )?;
                let staging = sibling_staging_path(&output)?;
                let plan = if has_video {
                    build_portrait_command_with_layout(
                        ffmpeg,
                        &source_path,
                        &staging,
                        request.profile,
                        runtime.h264_nvenc_runtime,
                        request.layout.clone().into(),
                    )?
                } else {
                    let title = request
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(&manifest.name);
                    self.build_audio_portrait_plan(
                        ffmpeg,
                        &source_path,
                        &staging,
                        request.profile,
                        runtime.h264_nvenc_runtime,
                        title,
                        request.variation,
                    )?
                };
                self.execute_render_plan(
                    job_id,
                    &request.project_id,
                    &plan,
                    Some(probe.duration_us),
                    0.06,
                    0.90,
                    cancel,
                    callback,
                )?;
                publish_atomic(&staging, &output, |path| {
                    let output_probe = probe_media(path, ffprobe)?;
                    if output_probe.primary_video_stream.is_none() || output_probe.duration_us <= 0
                    {
                        return Err(MediaError::new(
                            "invalid_render",
                            "The rendered output is not a playable video",
                        ));
                    }
                    Ok(())
                })?;
            } else {
                let existing = probe_media(&output, ffprobe)?;
                if existing.primary_video_stream.is_none() {
                    return Err(VideoServiceError::new(
                        "video.cache_invalid",
                        "The existing render cache entry is not playable",
                    ));
                }
            }
            self.store
                .put_video_cache(
                    &cache_key,
                    cache_namespace,
                    Some(&request.project_id),
                    &json!({ "source_sha256": source.sha256, "profile": profile_value }),
                    &output,
                )
                .map_err(VideoServiceError::store)?;
            output
        };
        let output_path = self.resolve_absolute_managed_path(&output_path)?;
        self.ensure_not_cancelled(cancel)?;
        let output_probe = probe_media(&output_path, ffprobe)?;
        let checksum = sha256_file(&output_path)?;
        let video_stream = output_probe.primary_video_stream.and_then(|index| {
            output_probe
                .streams
                .iter()
                .find(|stream| stream.index == index)
        });
        let (width, height) = video_stream
            .and_then(|stream| stream.width.zip(stream.height))
            .unwrap_or_else(|| portrait_dimensions(request.profile));
        let artifact_role = if request.profile == RenderProfile::Final {
            RenderArtifactRole::FinalMaster
        } else {
            RenderArtifactRole::Preview
        };
        let artifact = RenderArtifact {
            id: new_id(),
            role: artifact_role,
            scene_id: None,
            managed_path: self.relative_managed_path(&output_path)?,
            sha256: checksum.clone(),
            cache_key: cache_key.clone(),
            mime_type: "video/mp4".to_string(),
            duration_us: Some(Microseconds(output_probe.duration_us)),
            width: Some(width),
            height: Some(height),
            publication_state: PublicationState::Published,
            created_at: utc_now(),
        };
        artifact.validate()?;
        let has_render_artifact = manifest
            .render_artifacts
            .iter()
            .any(|existing| existing.cache_key == cache_key);
        let target_revision = if has_render_artifact {
            expectation.revision
        } else {
            expectation.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The portrait output version could not be advanced",
                )
            })?
        };
        let output_id = stable_output_id(
            &request.project_id,
            target_revision,
            &semantic_role,
            &cache_key,
            request.variation,
        );
        let build_output_request = |version_id: Option<&str>| {
            json!({
                "id": output_id,
                "project_id": request.project_id,
                "version_id": version_id,
                "job_id": job_id,
                "kind": output_kind,
                "label": match (request.profile, request.variation) {
                    (RenderProfile::Final, 0) => "Final master".to_string(),
                    (RenderProfile::Final, variation) => format!("Final variation {variation}"),
                    (_, 0) => "Video preview".to_string(),
                    (_, variation) => format!("Preview variation {variation}"),
                },
                "artifact_path": output_path,
                "mime_type": "video/mp4",
                "sha256": checksum,
                "duration_us": output_probe.duration_us,
                "width": width,
                "height": height,
                "is_primary": request.profile == RenderProfile::Final && request.variation == 0,
                "provenance": {
                    "producer": "soundAr Video Studio",
                    "producer_version": SERVICE_VERSION,
                    "manifest_revision": expectation.revision,
                    "source_version_id": expectation.version_id,
                    "request_sha256": request_sha256,
                    "source_asset_id": source.id,
                    "cache_key": cache_key,
                    "profile": request.profile,
                    "layout": request.layout,
                    "variation": request.variation,
                    "audio_only": !has_video,
                },
            })
        };
        let publication_lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(&current, &expectation)?;
        #[cfg(test)]
        if !has_render_artifact {
            self.trigger_single_render_test_failpoint(
                SingleRenderTestFailpoint::PortraitBeforeAtomicPublication,
            )?;
        }
        let (committed, output) = if has_render_artifact {
            let request_value = build_output_request(Some(&expectation.version_id));
            let output = self
                .store
                .publish_video_output_current_cancellable(
                    &request_value,
                    expectation.revision,
                    &expectation.version_id,
                    &publication_lock.token,
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            (current, output)
        } else {
            let actor = require_text(
                &request.actor,
                "video.invalid_actor",
                "An actor is required",
            )?;
            let mut next_manifest = manifest.clone();
            if i64::try_from(next_manifest.revision).ok() != Some(expectation.revision) {
                return Err(VideoServiceError::new(
                    "video.revision_integrity_failed",
                    "The frozen portrait manifest and project revision are not aligned",
                ));
            }
            let next_revision = next_manifest.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The portrait artifact revision could not be advanced",
                )
            })?;
            next_manifest.render_artifacts.push(artifact);
            let created_at = utc_now();
            let parent_id = next_manifest
                .revision_history
                .last()
                .map(|record| record.id.clone());
            next_manifest.revision = next_revision;
            next_manifest.updated_at = created_at.clone();
            next_manifest.revision_history.push(RevisionRecord {
                id: new_id(),
                revision: next_revision,
                parent_id,
                actor: actor.to_string(),
                reason: reason.to_string(),
                changed_paths: vec!["/render_artifacts".to_string()],
                invalidated_stages: BTreeSet::from([invalidated_stage]),
                created_at,
            });
            next_manifest.validate_strict()?;
            let output_request = build_output_request(None);
            let committed = self
                .store
                .commit_video_manifest_with_outputs_cancellable(
                    &request.project_id,
                    expectation.revision,
                    &serde_json::to_value(&next_manifest).map_err(json_error)?,
                    actor,
                    reason,
                    &publication_lock.token,
                    Some(if request.profile == RenderProfile::Final {
                        "completed"
                    } else {
                        "ready"
                    }),
                    &[output_request],
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            let output = committed
                .get("outputs")
                .and_then(Value::as_array)
                .and_then(|outputs| {
                    outputs.iter().find(|output| {
                        output.get("id").and_then(Value::as_str) == Some(output_id.as_str())
                    })
                })
                .cloned()
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.store_contract_failed",
                        "The atomically published portrait output could not be reloaded",
                    )
                })?;
            (committed, output)
        };
        #[cfg(test)]
        self.trigger_single_render_test_failpoint(
            SingleRenderTestFailpoint::PortraitAfterAtomicPublication,
        )?;
        let committed_expectation = project_expectation(&committed)?;
        ensure_project_matches(
            &self.get_project(&request.project_id)?,
            &committed_expectation,
        )?;
        drop(publication_lock);
        let version_id = Some(committed_expectation.version_id.as_str());
        self.checkpoint_stage(
            &request.project_id,
            version_id,
            stage_key,
            &format!("variation-{}", request.variation),
            job_id,
            "completed",
            resource_class_name(resources.class),
            1.0,
            &cache_key,
            Some(&checksum),
            json!({ "output_id": output.get("id"), "cache_hit": cache_hit }),
            None,
        )?;
        let wall_seconds = started.elapsed().as_secs_f64();
        let media_seconds = output_probe.duration_us as f64 / 1_000_000.0;
        let _ = self.store.record_video_performance(&json!({
            "project_id": request.project_id,
            "job_id": job_id,
            "operation": if request.profile == RenderProfile::Final { "final_render" } else { "preview_render" },
            "profile": format!("{:?}", request.profile).to_ascii_lowercase(),
            "wall_seconds": wall_seconds,
            "media_seconds": media_seconds,
            "realtime_factor": if media_seconds > 0.0 { Some(wall_seconds / media_seconds) } else { None },
            "cache_hit": cache_hit,
            "details": { "encoder": if runtime.h264_nvenc_runtime { "nvenc_with_software_fallback" } else { "libx264" }, "audio_only": !has_video },
        }));
        let _ = self.store.update_job(job_id, "running", 0.99);
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "published",
            0.99,
            if request.profile == RenderProfile::Final {
                "Final master is ready to play"
            } else {
                "Preview is ready to play"
            },
            Some(output),
            Some(json!({ "cache_hit": cache_hit, "wall_seconds": wall_seconds })),
        );
        Ok(())
    }

    fn perform_narration_replacement(
        &self,
        job_id: &str,
        request: &ReplaceNarrationRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        self.ensure_not_cancelled(cancel)?;
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let expectation = ProjectExpectation {
            revision: request.expected_revision,
            version_id: request.expected_version_id.clone(),
        };
        let project = self.get_project(&request.project_id)?;
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        if let Err(conflict) = ensure_project_matches(&project, &expectation) {
            if narration_replacements_applied(&manifest, &request.replacements) {
                self.store
                    .update_job(job_id, "running", 0.99)
                    .map_err(VideoServiceError::store)?;
                emit_progress(
                    callback,
                    job_id,
                    &request.project_id,
                    "narration_ready",
                    0.99,
                    "The requested narration revision was already applied",
                    None,
                    Some(json!({ "idempotent_replay": true })),
                );
                return Ok(());
            }
            return Err(conflict);
        }
        if narration_replacements_applied(&manifest, &request.replacements) {
            self.store
                .update_job(job_id, "running", 0.99)
                .map_err(VideoServiceError::store)?;
            return Ok(());
        }

        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to conform generated narration",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate generated narration",
        )?;
        let resources = ResourceRequest {
            class: ResourceClass::Medium,
            vram_mb: 0,
            cpu_threads: 4,
            io_slots: 2,
            nvenc_sessions: 0,
        };
        let _resource_lease =
            self.acquire_resources(job_id, &request.project_id, resources, cancel, callback)?;
        let request_sha = sha256_bytes(serde_json::to_vec(request).map_err(json_error)?.as_slice());
        self.checkpoint_stage(
            &request.project_id,
            Some(&expectation.version_id),
            "speech",
            "narration-replacement",
            job_id,
            "running",
            resource_class_name(resources.class),
            0.05,
            &request_sha,
            None,
            json!({ "replacement_count": request.replacements.len() }),
            None,
        )?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "conforming_narration",
            0.05,
            "Preparing the new voice takes for the existing scene timing",
            None,
            None,
        );

        let mut prepared = Vec::with_capacity(request.replacements.len());
        let mut prepared_clip_ids = BTreeSet::new();
        let mut prepared_scene_ids = BTreeSet::new();
        let mut prepared_turn_ids = BTreeSet::new();
        for (index, replacement) in request.replacements.iter().enumerate() {
            self.ensure_not_cancelled(cancel)?;
            let existing_binding = if let Some(binding_id) = replacement.binding_id.as_deref() {
                Some(
                    manifest
                        .narration_bindings
                        .iter()
                        .find(|binding| binding.id == binding_id)
                        .cloned()
                        .ok_or_else(|| {
                            VideoServiceError::new(
                                "video.narration_target_not_found",
                                "The selected narration binding no longer exists",
                            )
                        })?,
                )
            } else if let Some(turn_id) = replacement.turn_id.as_deref() {
                manifest
                    .narration_bindings
                    .iter()
                    .find(|binding| binding.turn_id.as_deref() == Some(turn_id))
                    .cloned()
            } else if let Some(scene_id) = replacement.scene_id.as_deref() {
                manifest
                    .narration_bindings
                    .iter()
                    .find(|binding| binding.scene_id.as_deref() == Some(scene_id))
                    .cloned()
            } else if let Some(clip_id) = replacement.clip_id.as_deref() {
                let artifact_id = manifest
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .find(|clip| clip.id == clip_id)
                    .and_then(|clip| clip.media.render_artifact_id.as_deref());
                artifact_id.and_then(|artifact_id| {
                    manifest
                        .narration_bindings
                        .iter()
                        .find(|binding| binding.render_artifact_id == artifact_id)
                        .cloned()
                })
            } else {
                None
            };
            let candidate_clips = manifest
                .tracks
                .iter()
                .filter(|track| matches!(track.kind, TrackKind::Audio))
                .flat_map(|track| track.clips.iter())
                .filter(|clip| {
                    if let Some(clip_id) = replacement.clip_id.as_deref() {
                        return clip.id == clip_id;
                    }
                    if let Some(turn_id) = replacement.turn_id.as_deref() {
                        return clip.turn_id.as_deref() == Some(turn_id);
                    }
                    if let Some(binding) = existing_binding.as_ref() {
                        return clip.media.render_artifact_id.as_deref()
                            == Some(binding.render_artifact_id.as_str());
                    }
                    replacement
                        .scene_id
                        .as_deref()
                        .is_some_and(|scene_id| clip.scene_id.as_deref() == Some(scene_id))
                })
                .cloned()
                .collect::<Vec<_>>();
            // A line that has never been performed has no clip to replace. That is the ordinary first
            // narration of a script, not a missing target, so its clip is created below at the take's
            // own measured length once the audio has been probed.
            let existing_clip = match candidate_clips.as_slice() {
                [clip] => Some(clip.clone()),
                [] if replacement.turn_id.is_some() => None,
                [] => {
                    return Err(VideoServiceError::new(
                        "video.narration_target_not_found",
                        "The selected scene has no exact audio clip to replace",
                    ))
                }
                _ => return Err(VideoServiceError::new(
                    "video.narration_target_ambiguous",
                    "The selected scene has multiple audio clips; choose the exact narration clip",
                )),
            };
            let placement_start_us = match &existing_clip {
                Some(clip) => clip.timeline_start_us,
                // Appended after everything already spoken, plus this line's own beat, so the
                // conversation is laid out in the order and timing the script asks for.
                None => {
                    let spoken_end = manifest
                        .tracks
                        .iter()
                        .filter(|track| matches!(track.kind, TrackKind::Audio))
                        .flat_map(|track| track.clips.iter())
                        .filter(|clip| clip.turn_id.is_some())
                        .map(|clip| clip.timeline_start_us.0 + clip.timeline_duration_us.0)
                        .max()
                        .unwrap_or(0);
                    let beat = replacement
                        .turn_id
                        .as_deref()
                        .and_then(|turn_id| {
                            manifest
                                .turn_beats
                                .iter()
                                .find(|beat| beat.turn_id == turn_id)
                        })
                        .map(|beat| beat.lead_in_us.0 - beat.overlap_us.0)
                        .unwrap_or(0);
                    Microseconds((spoken_end + beat).max(0))
                }
            };
            let placeholder_clip_id = existing_clip
                .as_ref()
                .map(|clip| clip.id.clone())
                .unwrap_or_else(|| {
                    format!(
                        "dialogue-clip-{}",
                        replacement.turn_id.as_deref().unwrap_or_default()
                    )
                });
            if !prepared_clip_ids.insert(placeholder_clip_id.clone()) {
                return Err(VideoServiceError::new(
                    "video.duplicate_narration_target",
                    "Two replacements resolved to the same narration clip",
                ));
            }
            let scene_id = replacement
                .scene_id
                .clone()
                .or_else(|| {
                    existing_binding
                        .as_ref()
                        .and_then(|binding| binding.scene_id.clone())
                })
                .or_else(|| {
                    existing_clip
                        .as_ref()
                        .and_then(|clip| clip.scene_id.clone())
                });
            if replacement.scene_id.is_some()
                && existing_clip
                    .as_ref()
                    .is_some_and(|clip| clip.scene_id.as_deref() != replacement.scene_id.as_deref())
            {
                return Err(VideoServiceError::new(
                    "video.narration_target_mismatch",
                    "The selected clip does not belong to the requested scene",
                ));
            }
            if let Some(scene_id) = scene_id.as_deref() {
                if !prepared_scene_ids.insert(scene_id.to_string()) {
                    return Err(VideoServiceError::new(
                        "video.duplicate_narration_target",
                        "A scene can receive only one narration take per revision",
                    ));
                }
            }
            let scene = scene_id
                .as_deref()
                .map(|scene_id| {
                    manifest
                        .reviewed_scenes
                        .iter()
                        .find(|scene| scene.id == scene_id)
                        .ok_or_else(|| {
                            VideoServiceError::new(
                                "video.narration_target_not_found",
                                "The narration scene no longer exists",
                            )
                        })
                })
                .transpose()?;

            // A turn-scoped replacement re-reads exactly one line. Resolving the turn from the
            // request, the existing binding, or the clip keeps a repeated request idempotent
            // whichever of those the caller had available.
            let turn_id = replacement
                .turn_id
                .clone()
                .or_else(|| {
                    existing_binding
                        .as_ref()
                        .and_then(|binding| binding.turn_id.clone())
                })
                .or_else(|| existing_clip.as_ref().and_then(|clip| clip.turn_id.clone()));
            if replacement.turn_id.is_some()
                && existing_clip
                    .as_ref()
                    .is_some_and(|clip| clip.turn_id.as_deref() != replacement.turn_id.as_deref())
            {
                return Err(VideoServiceError::new(
                    "video.narration_target_mismatch",
                    "The selected clip does not carry the requested dialogue turn",
                ));
            }
            if let Some(turn_id) = turn_id.as_deref() {
                if !prepared_turn_ids.insert(turn_id.to_string()) {
                    return Err(VideoServiceError::new(
                        "video.duplicate_narration_target",
                        "A dialogue turn can receive only one narration take per revision",
                    ));
                }
            }
            let turn = turn_id
                .as_deref()
                .map(|turn_id| {
                    manifest
                        .dialogue
                        .iter()
                        .find(|turn| turn.id == turn_id)
                        .ok_or_else(|| {
                            VideoServiceError::new(
                                "video.narration_target_not_found",
                                "The narration dialogue turn no longer exists",
                            )
                        })
                })
                .transpose()?;
            // The character owns the route. Accepting a take recorded against a different
            // speaker would break the guarantee that reassigning one character's voice
            // invalidates exactly that character's takes.
            if let Some(turn) = turn {
                let member = manifest
                    .cast
                    .iter()
                    .find(|member| member.id == turn.character_id)
                    .ok_or_else(|| {
                        VideoServiceError::new(
                            "video.narration_target_not_found",
                            "The character who speaks this turn is no longer in the cast",
                        )
                    })?;
                // The engine speaker is the voice route, not a person; what must match the
                // character is the route their cast entry declares.
                if replacement.voice_id != member.voice_id
                    || replacement.model_id != member.model_id
                {
                    return Err(VideoServiceError::new(
                        "video.narration_route_mismatch",
                        "The replacement voice route does not match the character who speaks this turn",
                    ));
                }
            }
            // The lexicon rewrites the line before a voice speaks it, so the generated audio is
            // checked against the spoken text while the take still records the words the writer
            // actually wrote.
            let lexicon_fingerprint = turn.and_then(|turn| {
                super::lexicon::fingerprint_for_character(&manifest.lexicon, &turn.character_id)
            });

            let history = self
                .store
                .get_history(&replacement.history_id)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.history_not_found",
                        "The generated speech take is no longer in soundAr History",
                    )
                })?;
            if history.get("artifact_state").and_then(Value::as_str) != Some("available") {
                return Err(VideoServiceError::new(
                    "video.history_artifact_unavailable",
                    "The selected History speech artifact is missing or changed",
                ));
            }
            if history.get("model_id").and_then(Value::as_str)
                != Some(replacement.model_id.as_str())
            {
                return Err(VideoServiceError::new(
                    "video.narration_route_mismatch",
                    "The generated speech model does not match the requested voice route",
                ));
            }
            let source_path = PathBuf::from(value_string(&history, "audio_path")?);
            let registered = self
                .store
                .get_registered_history_by_audio_path(source_path.to_str().ok_or_else(|| {
                    VideoServiceError::new(
                        "video.invalid_history_artifact",
                        "The History artifact path is not valid UTF-8",
                    )
                })?)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.history_artifact_unregistered",
                        "The selected speech take is not an integrity-bound History artifact",
                    )
                })?;
            if registered.get("id").and_then(Value::as_str) != Some(replacement.history_id.as_str())
            {
                return Err(VideoServiceError::new(
                    "video.history_artifact_mismatch",
                    "The selected History id does not own the speech artifact",
                ));
            }
            let script = turn
                .map(|turn| turn.spoken_text())
                .or_else(|| scene.map(|scene| scene.script.as_str()))
                .unwrap_or_else(|| history.get("text").and_then(Value::as_str).unwrap_or(""));
            let script_sha = sha256_bytes(script.as_bytes());
            // What the engine was asked to say. Identical to the written line unless a
            // pronunciation rule governs this character.
            let spoken = turn
                .map(|turn| {
                    super::lexicon::apply_lexicon(
                        turn.spoken_text(),
                        &super::lexicon::effective_entries(&manifest.lexicon, &turn.character_id),
                    )
                    .spoken_text
                })
                .unwrap_or_else(|| script.to_string());
            let spoken_sha = sha256_bytes(spoken.as_bytes());
            if (turn.is_some() || scene.is_some())
                && history
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| sha256_bytes(text.as_bytes()))
                    .as_deref()
                    != Some(spoken_sha.as_str())
            {
                return Err(VideoServiceError::new(
                    "video.narration_script_mismatch",
                    "The generated speech does not match the words this scene or turn must speak",
                ));
            }
            let source_probe = probe_media(&source_path, ffprobe)?;
            if source_probe.primary_audio_stream.is_none() || source_probe.duration_us <= 0 {
                return Err(VideoServiceError::new(
                    "video.invalid_history_artifact",
                    "The selected History item is not playable audio",
                ));
            }
            validate_media_duration(source_probe.duration_us)?;
            let source_sha = sha256_file_with_cancel(&source_path, Some(cancel))?;
            // A replaced line keeps its existing slot so the timeline does not move under it; a
            // first performance takes the take's own measured length.
            let target_duration_us = existing_clip
                .as_ref()
                .map_or(source_probe.duration_us, |clip| clip.timeline_duration_us.0);
            let cache_key = CacheKeyBuilder::new(
                CacheStage::Speech,
                format!("{SERVICE_VERSION}:narration-conform-v1"),
            )
            .artifact("history_speech", source_sha.clone())
            .tool_version(
                "ffmpeg",
                runtime
                    .ffmpeg
                    .version
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            )
            .manifest_slice(json!({
                "clip_id": placeholder_clip_id,
                "scene_id": scene_id,
                "script_sha256": script_sha,
                "timeline_duration_us": target_duration_us,
            }))
            .profile(json!({
                "format": "pcm_s16le_48000_stereo_wav",
                "voice_id": replacement.voice_id,
                "model_id": replacement.model_id,
                "speaker": replacement.speaker,
                "language": replacement.language,
            }))
            .build()?
            .into_string();
            let mut cache_hit = false;
            let conformed_path = if let Some(cache) = self
                .store
                .get_video_cache(&cache_key)
                .map_err(VideoServiceError::store)?
            {
                cache_hit = true;
                self.resolve_absolute_managed_path(&PathBuf::from(value_string(
                    &cache,
                    "artifact_path",
                )?))?
            } else {
                let output = self.cache_path("narration", &cache_key, "wav")?;
                if !output.is_file() {
                    let estimated_bytes = u64::try_from(target_duration_us)
                        .unwrap_or_default()
                        .saturating_mul(192_000)
                        / 1_000_000;
                    let _storage_lease = self.reserve_storage(
                        format!("{job_id}:narration:{cache_key}"),
                        &self.video_root,
                        with_disk_headroom(estimated_bytes.max(1_048_576), 1),
                        "narration_conform",
                    )?;
                    let staging = sibling_staging_path(&output)?;
                    let plan = self.build_narration_conform_plan(
                        ffmpeg,
                        &source_path,
                        &staging,
                        source_probe.duration_us,
                        target_duration_us,
                    )?;
                    let progress_start =
                        0.08 + (index as f64 / request.replacements.len() as f64) * 0.7;
                    let progress_end =
                        0.08 + ((index + 1) as f64 / request.replacements.len() as f64) * 0.7;
                    self.execute_render_plan(
                        job_id,
                        &request.project_id,
                        &plan,
                        Some(target_duration_us),
                        progress_start,
                        progress_end,
                        cancel,
                        callback,
                    )?;
                    publish_atomic(&staging, &output, |path| {
                        let probe = probe_media(path, ffprobe)?;
                        if probe.primary_audio_stream.is_none()
                            || (probe.duration_us - target_duration_us).abs() > 50_000
                        {
                            return Err(MediaError::new(
                                "narration_duration_mismatch",
                                "The conformed narration does not match the scene clock",
                            ));
                        }
                        Ok(())
                    })?;
                }
                secure_managed_file(&output)?;
                self.store
                    .put_video_cache(
                        &cache_key,
                        "narration",
                        Some(&request.project_id),
                        &json!({
                            "history_id": replacement.history_id,
                            "source_sha256": source_sha,
                            "duration_us": target_duration_us,
                        }),
                        &output,
                    )
                    .map_err(VideoServiceError::store)?;
                output
            };
            let conformed_probe = probe_media(&conformed_path, ffprobe)?;
            if conformed_probe.primary_audio_stream.is_none()
                || (conformed_probe.duration_us - target_duration_us).abs() > 50_000
            {
                return Err(VideoServiceError::new(
                    "video.cache_invalid",
                    "The cached narration no longer matches its scene timing",
                ));
            }
            let conformed_sha = sha256_file_with_cancel(&conformed_path, Some(cancel))?;
            let existing_artifact = manifest
                .render_artifacts
                .iter()
                .find(|artifact| artifact.cache_key == cache_key)
                .cloned();
            let artifact = existing_artifact.unwrap_or(RenderArtifact {
                id: new_id(),
                role: RenderArtifactRole::SceneSegment,
                scene_id: scene_id.clone(),
                managed_path: self.relative_managed_path(&conformed_path)?,
                sha256: conformed_sha.clone(),
                cache_key: cache_key.clone(),
                mime_type: "audio/wav".to_string(),
                duration_us: Some(Microseconds(target_duration_us)),
                width: None,
                height: None,
                publication_state: PublicationState::Published,
                created_at: utc_now(),
            });
            artifact.validate()?;
            if artifact.sha256 != conformed_sha
                || artifact.duration_us != Some(Microseconds(target_duration_us))
            {
                return Err(VideoServiceError::new(
                    "video.cache_collision",
                    "A narration cache key resolved to different media",
                ));
            }
            let generation_job_id = value_string(&history, "job_id")?;
            let binding = NarrationBinding {
                id: existing_binding
                    .as_ref()
                    .map(|binding| binding.id.clone())
                    .unwrap_or_else(new_id),
                scene_id: scene_id.clone(),
                turn_id: turn_id.clone(),
                // The character comes from the turn itself, so a take can never claim a character
                // other than the one whose line it performs.
                character_id: turn.map(|turn| turn.character_id.clone()),
                lexicon_fingerprint: lexicon_fingerprint.clone(),
                fidelity: replacement.fidelity,
                render_artifact_id: artifact.id.clone(),
                history_id: replacement.history_id.clone(),
                generation_job_id,
                voice_id: replacement.voice_id.clone(),
                model_id: replacement.model_id.clone(),
                speaker: replacement.speaker.clone(),
                language: replacement.language.clone(),
                script_sha256: script_sha,
                performance: replacement.performance.clone(),
                created_at: utc_now(),
            };
            binding.validate()?;
            self.store
                .upsert_video_asset(&json!({
                    "id": artifact.id,
                    "project_id": request.project_id,
                    "kind": "speech",
                    "source_kind": "derived",
                    "local_path": conformed_path,
                    "mime_type": "audio/wav",
                    "content_sha256": conformed_sha,
                    "size_bytes": fs::metadata(&conformed_path).map(|metadata| metadata.len() as i64).ok(),
                    "duration_us": target_duration_us,
                    "status": "ready",
                    "probe": conformed_probe,
                    "provenance": {
                        "producer": "soundAr Video Studio narration revision",
                        "producer_version": SERVICE_VERSION,
                        "history_id": replacement.history_id,
                        "generation_job_id": binding.generation_job_id,
                        "voice_id": replacement.voice_id,
                        "model_id": replacement.model_id,
                        "speaker": replacement.speaker,
                        "language": replacement.language,
                        "cache_key": cache_key,
                    },
                }))
                .map_err(VideoServiceError::store)?;
            emit_progress(
                callback,
                job_id,
                &request.project_id,
                "narration_take_ready",
                0.08 + ((index + 1) as f64 / request.replacements.len() as f64) * 0.7,
                "A replacement narration take is ready",
                Some(json!({
                    "kind": "narration",
                    "artifact_path": conformed_path,
                    "mime_type": "audio/wav",
                    "duration_us": target_duration_us,
                })),
                Some(json!({ "scene_id": scene_id, "cache_hit": cache_hit })),
            );
            prepared.push(PreparedNarrationReplacement {
                clip_id: placeholder_clip_id.clone(),
                // Present when this line has never been performed, so the commit places it rather
                // than looking for a clip that does not exist.
                new_clip: existing_clip.is_none().then(|| TimelineClip {
                    id: placeholder_clip_id,
                    scene_id: scene_id.clone(),
                    turn_id: replacement.turn_id.clone(),
                    media: super::MediaReference {
                        source_asset_id: None,
                        render_artifact_id: Some(artifact.id.clone()),
                    },
                    source_range: TimeRange::new(0, target_duration_us)
                        .unwrap_or(TimeRange::new(0, 1).expect("a positive minimal range")),
                    timeline_start_us: placement_start_us,
                    timeline_duration_us: Microseconds(target_duration_us),
                    playback_rate: RationalRate::ONE,
                    gain_db_milli: 0,
                    muted: false,
                    crop: None,
                }),
                replaced_binding_id: existing_binding.map(|binding| binding.id),
                artifact,
                binding,
            });
        }

        self.ensure_not_cancelled(cancel)?;
        if let Some(parent_job_id) = request.parent_job_id.as_deref() {
            let parent = self
                .store
                .get_job(parent_job_id)
                .map_err(VideoServiceError::store)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.parent_job_not_found",
                        "The narration revision parent task no longer exists",
                    )
                })?;
            match parent.get("status").and_then(Value::as_str) {
                Some("queued" | "preparing" | "running") => {}
                Some("cancelled") => return Err(VideoServiceError::cancelled()),
                _ => {
                    return Err(VideoServiceError::new(
                        "video.parent_job_inactive",
                        "The narration revision parent task is no longer active",
                    ))
                }
            }
        }
        self.ensure_not_cancelled(cancel)?;
        ensure_project_matches(&self.get_project(&request.project_id)?, &expectation)?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "narration_committing",
            0.9,
            "Committing the revised narration to the current timeline",
            None,
            None,
        );
        let committed = self.commit_manifest_mutation_at_if_parent_active(
            &request.project_id,
            &expectation,
            &request.actor,
            "Regenerated selected narration with a revised voice",
            Some("ready"),
            vec![
                "/tracks".to_string(),
                "/render_artifacts".to_string(),
                "/narration_bindings".to_string(),
            ],
            BTreeSet::from([
                RevisionStage::Speech,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            request.parent_job_id.as_deref(),
            move |manifest| {
                for replacement in &prepared {
                    let mut found = false;
                    for clip in manifest
                        .tracks
                        .iter_mut()
                        .filter(|track| matches!(track.kind, TrackKind::Audio))
                        .flat_map(|track| track.clips.iter_mut())
                    {
                        if clip.id == replacement.clip_id {
                            clip.media.source_asset_id = None;
                            clip.media.render_artifact_id = Some(replacement.artifact.id.clone());
                            clip.source_range = TimeRange::new(0, clip.timeline_duration_us.0)?;
                            clip.playback_rate = RationalRate::ONE;
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        // A first performance places its line rather than rewriting one, on a
                        // dedicated dialogue track so it never disturbs imported source audio.
                        let Some(new_clip) = replacement.new_clip.clone() else {
                            return Err(VideoServiceError::new(
                                "video.narration_target_not_found",
                                "The narration clip changed before the revision was committed",
                            ));
                        };
                        let end = new_clip.timeline_start_us.0 + new_clip.timeline_duration_us.0;
                        if end > manifest.timeline_duration_us.0 {
                            let previous_end = manifest.timeline_duration_us.0;
                            manifest.timeline_duration_us = Microseconds(end);
                            // A gap-preserving track must explicitly cover the whole timeline, so
                            // lengthening the episode extends each one with a declared gap rather
                            // than leaving an implicit hole the renderer would have to interpret.
                            let extended = manifest
                                .tracks
                                .iter()
                                .filter(|track| track.preserve_gaps)
                                .map(|track| track.id.clone())
                                .collect::<Vec<_>>();
                            for track_id in extended {
                                manifest.gaps.push(TimelineGap {
                                    id: format!("gap-{track_id}-{previous_end:012}"),
                                    track_id,
                                    range: TimeRange::new(previous_end, end)?,
                                    reason: GapReason::Padding,
                                    source_asset_id: None,
                                    source_range: None,
                                });
                            }
                        }
                        match manifest
                            .tracks
                            .iter_mut()
                            .find(|track| track.id == DIALOGUE_TRACK_ID)
                        {
                            Some(track) => track.clips.push(new_clip),
                            None => manifest.tracks.push(TimelineTrack {
                                id: DIALOGUE_TRACK_ID.to_string(),
                                kind: TrackKind::Audio,
                                clips: vec![new_clip],
                                // Dialogue occupies only the moments its lines are spoken, so this
                                // track deliberately does not partition the timeline.
                                preserve_gaps: false,
                            }),
                        }
                    }
                    if !manifest
                        .render_artifacts
                        .iter()
                        .any(|artifact| artifact.id == replacement.artifact.id)
                    {
                        manifest.render_artifacts.push(replacement.artifact.clone());
                    }
                    manifest.narration_bindings.retain(|binding| {
                        if replacement.replaced_binding_id.as_deref() == Some(binding.id.as_str()) {
                            return false;
                        }
                        // A line's take replaces that line's previous take; a scene's replaces that
                        // scene's. Matching on the wrong one would drop an unrelated take, and a
                        // scene-scoped binding has `None` here, which must not collide.
                        match replacement.binding.turn_id.as_deref() {
                            Some(turn_id) => binding.turn_id.as_deref() != Some(turn_id),
                            None => {
                                replacement.binding.scene_id.is_none()
                                    || replacement.binding.scene_id.as_deref()
                                        != binding.scene_id.as_deref()
                            }
                        }
                    });
                    manifest
                        .narration_bindings
                        .push(replacement.binding.clone());
                }
                Ok(())
            },
        )?;
        let committed_expectation = project_expectation(&committed)?;
        self.checkpoint_stage(
            &request.project_id,
            Some(&committed_expectation.version_id),
            "speech",
            "narration-replacement",
            job_id,
            "completed",
            resource_class_name(resources.class),
            1.0,
            &request_sha,
            Some(&committed_expectation.version_id),
            json!({
                "revision": committed_expectation.revision,
                "version_id": committed_expectation.version_id,
                "replacement_count": request.replacements.len(),
            }),
            None,
        )?;
        self.store
            .update_job(job_id, "running", 0.99)
            .map_err(VideoServiceError::store)?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "narration_ready",
            0.99,
            "The new voice is in the timeline; only affected renders need rebuilding",
            Some(json!({
                "kind": "video_project",
                "project_id": request.project_id,
                "revision": committed_expectation.revision,
                "version_id": committed_expectation.version_id,
            })),
            Some(json!({ "replacement_count": request.replacements.len() })),
        );
        Ok(())
    }

    fn build_narration_conform_plan(
        &self,
        ffmpeg: &Path,
        input: &Path,
        output: &Path,
        input_duration_us: i64,
        target_duration_us: i64,
    ) -> ServiceResult<RenderCommandPlan> {
        if input_duration_us <= 0 || target_duration_us <= 0 {
            return Err(VideoServiceError::new(
                "video.invalid_narration_duration",
                "Narration source and target durations must be positive",
            ));
        }
        validate_media_duration(target_duration_us)?;
        let parent = output.parent().ok_or_else(|| {
            VideoServiceError::new(
                "video.invalid_render_output",
                "The narration output has no parent directory",
            )
        })?;
        self.secure_managed_directory(parent)?;
        if output.exists() {
            return Err(VideoServiceError::new(
                "video.render_output_exists",
                "The narration staging output already exists",
            ));
        }
        let tempo = input_duration_us as f64 / target_duration_us as f64;
        let filter = format!(
            "{},apad,atrim=duration={:.6},asetpts=N/SR/TB",
            atempo_filter_chain(tempo)?,
            target_duration_us as f64 / 1_000_000.0,
        );
        let mut args = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-nostdin"),
            OsString::from("-n"),
            OsString::from("-progress"),
            OsString::from("pipe:2"),
            OsString::from("-stats_period"),
            OsString::from("0.25"),
        ];
        args.extend(local_media_input_args(input)?);
        args.extend([
            OsString::from("-map"),
            OsString::from("0:a:0"),
            OsString::from("-vn"),
            OsString::from("-af"),
            OsString::from(filter),
            OsString::from("-c:a"),
            OsString::from("pcm_s16le"),
            OsString::from("-ar"),
            OsString::from("48000"),
            OsString::from("-ac"),
            OsString::from("2"),
            OsString::from("-f"),
            OsString::from("wav"),
            output.as_os_str().to_os_string(),
        ]);
        Ok(RenderCommandPlan {
            profile: RenderProfile::Preview,
            workload_class: RenderWorkloadClass::Medium,
            primary: RenderCommand {
                program: ffmpeg.to_path_buf(),
                args,
                output: output.to_path_buf(),
                encoder: VideoEncoder::Libx264,
                emits_progress: true,
            },
            software_fallback: None,
        })
    }

    fn perform_timeline_render(
        &self,
        job_id: &str,
        request: &TimelineRenderRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        self.run_timeline_render(job_id, request, false, cancel, callback)
            .map(|_| ())
    }

    fn perform_timeline_render_batch(
        &self,
        job_id: &str,
        request: &TimelineRenderBatchRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        let expectation = ProjectExpectation {
            revision: request.base.expected_revision,
            version_id: request.base.expected_version_id.clone(),
        };
        let frozen = self.get_project(&request.base.project_id)?;
        if let Err(conflict) = ensure_project_matches(&frozen, &expectation) {
            if self.timeline_batch_already_published(&frozen, job_id, request, cancel)? {
                self.store
                    .update_job(job_id, "running", 0.99)
                    .map_err(VideoServiceError::store)?;
                emit_progress(
                    callback,
                    job_id,
                    &request.base.project_id,
                    "published",
                    0.99,
                    "The complete variation batch was recovered after restart",
                    None,
                    Some(json!({
                        "idempotent_replay": true,
                        "variation_count": request.variations.len(),
                    })),
                );
                return Ok(());
            }
            return Err(conflict);
        }
        let frozen_manifest: VideoProjectManifest = serde_json::from_value(
            frozen
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        frozen_manifest.validate_strict()?;
        let mut prepared = Vec::with_capacity(request.variations.len());
        for variation in &request.variations {
            self.ensure_not_cancelled(cancel)?;
            let mut variation_request = request.base.clone();
            variation_request.variation = *variation;
            let output = self
                .run_timeline_render(job_id, &variation_request, true, cancel, callback)?
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.batch_render_failed",
                        "A deferred timeline render was published unexpectedly",
                    )
                })?;
            prepared.push(output);
        }
        self.ensure_not_cancelled(cancel)?;
        ensure_project_matches(&self.get_project(&request.base.project_id)?, &expectation)?;

        let existing_cache_keys = frozen_manifest
            .render_artifacts
            .iter()
            .map(|artifact| artifact.cache_key.as_str())
            .collect::<BTreeSet<_>>();
        let mut additions_by_cache = BTreeMap::<String, RenderArtifact>::new();
        for output in &prepared {
            for artifact in [&output.caption_artifact, &output.render_artifact] {
                if !existing_cache_keys.contains(artifact.cache_key.as_str()) {
                    additions_by_cache
                        .entry(artifact.cache_key.clone())
                        .or_insert_with(|| artifact.clone());
                }
            }
        }
        let reason = match request.base.profile {
            TimelineRenderProfile::Preview => "Published reviewed-timeline preview variations",
            TimelineRenderProfile::Final => "Published reviewed final-render variations",
        };
        let status = match request.base.profile {
            TimelineRenderProfile::Preview => "ready",
            TimelineRenderProfile::Final => "completed",
        };
        let invalidated_stage = match request.base.profile {
            TimelineRenderProfile::Preview => RevisionStage::Preview,
            TimelineRenderProfile::Final => RevisionStage::FinalRender,
        };
        let target_revision = if additions_by_cache.is_empty() {
            expectation.revision
        } else {
            expectation.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The batch output version could not be advanced",
                )
            })?
        };
        let build_output_requests = |version_id: Option<&str>| {
            prepared
                .iter()
                .map(|render| {
                let output_kind = match (render.request.profile, render.request.variation) {
                    (_, variation) if variation > 0 => "variation",
                    (TimelineRenderProfile::Preview, _) => "preview",
                    (TimelineRenderProfile::Final, _) => "master",
                };
                let semantic_role = match render.request.profile {
                    TimelineRenderProfile::Preview => "timeline-preview",
                    TimelineRenderProfile::Final => "timeline-final",
                };
                let output_id = stable_output_id(
                    &render.request.project_id,
                    target_revision,
                    semantic_role,
                    &render.render_key,
                    render.request.variation,
                );
                json!({
                    "id": output_id,
                    "project_id": render.request.project_id,
                    "version_id": version_id,
                    "job_id": job_id,
                    "kind": output_kind,
                    "label": match (render.request.profile, render.request.variation) {
                        (TimelineRenderProfile::Final, 0) => "Final master".to_string(),
                        (TimelineRenderProfile::Final, variation) => format!("Final variation {variation}"),
                        (TimelineRenderProfile::Preview, 0) => "Timeline preview".to_string(),
                        (TimelineRenderProfile::Preview, variation) => format!("Preview variation {variation}"),
                    },
                    "artifact_path": render.output_path,
                    "mime_type": "video/mp4",
                    "sha256": render.output_sha,
                    "duration_us": render.output_duration_us,
                    "width": render.width,
                    "height": render.height,
                    "is_primary": render.request.profile == TimelineRenderProfile::Final
                        && render.request.variation == 0,
                    "provenance": {
                        "producer": "soundAr Video Studio timeline assembler",
                        "producer_version": SERVICE_VERSION,
                        "manifest_revision": request.base.expected_revision,
                        "source_version_id": request.base.expected_version_id,
                        "render_cache_key": render.render_key,
                        "caption_cache_key": render.caption_key,
                        "profile": render.request.profile,
                        "variation": render.request.variation,
                        "caption_theme": render.request.caption_theme,
                        "portrait_layout": render.request.portrait_layout,
                        "batch_size": prepared.len(),
                        "nvenc_with_software_fallback": render.nvenc_with_software_fallback,
                    },
                })
            })
                .collect::<Vec<_>>()
        };
        let publication_lock =
            ProjectLock::acquire(self, &request.base.project_id, &request.base.actor)?;
        let current = self.get_project(&request.base.project_id)?;
        ensure_project_matches(&current, &expectation)?;
        let (committed, outputs) = if additions_by_cache.is_empty() {
            let output_requests = build_output_requests(Some(&expectation.version_id));
            let outputs = self
                .store
                .publish_video_outputs_current_cancellable(
                    &output_requests,
                    expectation.revision,
                    &expectation.version_id,
                    &publication_lock.token,
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            (current, outputs)
        } else {
            let actor = require_text(
                &request.base.actor,
                "video.invalid_actor",
                "An actor is required",
            )?;
            let mut next_manifest = frozen_manifest.clone();
            next_manifest
                .render_artifacts
                .extend(additions_by_cache.into_values());
            if i64::try_from(next_manifest.revision).ok() != Some(expectation.revision) {
                return Err(VideoServiceError::new(
                    "video.revision_integrity_failed",
                    "The frozen manifest and project revision are not aligned",
                ));
            }
            let next_revision = next_manifest.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The batch artifact revision could not be advanced",
                )
            })?;
            let created_at = utc_now();
            let parent_id = next_manifest
                .revision_history
                .last()
                .map(|record| record.id.clone());
            next_manifest.revision = next_revision;
            next_manifest.updated_at = created_at.clone();
            next_manifest.revision_history.push(RevisionRecord {
                id: new_id(),
                revision: next_revision,
                parent_id,
                actor: actor.to_string(),
                reason: reason.to_string(),
                changed_paths: vec!["/render_artifacts".to_string()],
                invalidated_stages: BTreeSet::from([invalidated_stage]),
                created_at,
            });
            next_manifest.validate_strict()?;
            let manifest_value = serde_json::to_value(&next_manifest).map_err(json_error)?;
            let output_requests = build_output_requests(None);
            let committed = self
                .store
                .commit_video_manifest_with_outputs_cancellable(
                    &request.base.project_id,
                    expectation.revision,
                    &manifest_value,
                    actor,
                    reason,
                    &publication_lock.token,
                    Some(status),
                    &output_requests,
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            let committed_expectation = project_expectation(&committed)?;
            if committed_expectation.revision != expectation.revision + 1
                || i64::try_from(next_manifest.revision).ok()
                    != Some(committed_expectation.revision)
            {
                return Err(VideoServiceError::new(
                    "video.revision_integrity_failed",
                    "The atomic batch revision did not advance exactly once",
                ));
            }
            let saved = committed
                .get("outputs")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_store_shape("outputs"))?;
            let outputs = prepared
                .iter()
                .map(|render| {
                    let kind = if render.request.variation > 0 {
                        "variation"
                    } else if render.request.profile == TimelineRenderProfile::Final {
                        "master"
                    } else {
                        "preview"
                    };
                    let semantic_role = match render.request.profile {
                        TimelineRenderProfile::Preview => "timeline-preview",
                        TimelineRenderProfile::Final => "timeline-final",
                    };
                    let expected_id = stable_output_id(
                        &render.request.project_id,
                        target_revision,
                        semantic_role,
                        &render.render_key,
                        render.request.variation,
                    );
                    let output = saved
                        .iter()
                        .find(|output| {
                            output.get("id").and_then(Value::as_str)
                                == Some(expected_id.as_str())
                        })
                        .ok_or_else(|| {
                            VideoServiceError::new(
                                "video.store_contract_failed",
                                "An atomically published semantic batch output could not be reloaded",
                            )
                            .details(json!({
                                "expected_output_id": expected_id,
                                "variation": render.request.variation,
                            }))
                        })?;
                    if output.get("version_id").and_then(Value::as_str)
                        != Some(committed_expectation.version_id.as_str())
                        || output.get("kind").and_then(Value::as_str) != Some(kind)
                        || output.get("sha256").and_then(Value::as_str)
                            != Some(render.output_sha.as_str())
                    {
                        return Err(VideoServiceError::new(
                            "video.store_contract_failed",
                            "An atomically published semantic batch output did not match its prepared artifact",
                        )
                        .details(json!({
                            "expected_output_id": expected_id,
                            "expected_version_id": committed_expectation.version_id,
                            "expected_kind": kind,
                            "expected_sha256": render.output_sha,
                        })));
                    }
                    Ok(output.clone())
                })
                .collect::<ServiceResult<Vec<_>>>()?;
            (committed, outputs)
        };
        let committed_expectation = project_expectation(&committed)?;
        ensure_project_matches(
            &self.get_project(&request.base.project_id)?,
            &committed_expectation,
        )?;
        drop(publication_lock);

        for (render, output) in prepared.iter().zip(&outputs) {
            self.checkpoint_stage(
                &request.base.project_id,
                Some(&committed_expectation.version_id),
                &render.stage_key,
                &render.scope_key,
                job_id,
                "completed",
                &render.resource_class,
                1.0,
                &render.render_key,
                Some(&render.output_sha),
                json!({
                    "output_id": output.get("id"),
                    "render_cache_hit": render.render_cache_hit,
                    "caption_cache_hit": render.caption_cache_hit,
                    "batch_size": outputs.len(),
                }),
                None,
            )?;
            let media_seconds = render.output_duration_us as f64 / 1_000_000.0;
            let _ = self.store.record_video_performance(&json!({
                "project_id": request.base.project_id,
                "job_id": job_id,
                "operation": if render.request.profile == TimelineRenderProfile::Final {
                    "timeline_final_render"
                } else {
                    "timeline_preview_render"
                },
                "profile": if render.request.profile == TimelineRenderProfile::Final { "final" } else { "preview" },
                "wall_seconds": render.wall_seconds,
                "media_seconds": media_seconds,
                "realtime_factor": if media_seconds > 0.0 {
                    Some(render.wall_seconds / media_seconds)
                } else {
                    None
                },
                "cache_hit": render.render_cache_hit,
                "details": {
                    "caption_cache_hit": render.caption_cache_hit,
                    "scene_count": render.scene_count,
                    "variation": render.request.variation,
                    "batch_size": outputs.len(),
                    "encoder": if render.nvenc_with_software_fallback {
                        "nvenc_with_software_fallback"
                    } else {
                        "libx264"
                    },
                },
            }));
        }
        self.store
            .update_job(job_id, "running", 0.99)
            .map_err(VideoServiceError::store)?;
        let prominent = outputs
            .iter()
            .zip(&prepared)
            .find(|(_, render)| render.request.variation == 0)
            .map(|(output, _)| output.clone())
            .or_else(|| outputs.first().cloned());
        emit_progress(
            callback,
            job_id,
            &request.base.project_id,
            "published",
            0.99,
            &format!("{} timeline variations are ready to play", outputs.len()),
            prominent,
            Some(json!({
                "variation_count": outputs.len(),
                "outputs": outputs,
                "revision": committed_expectation.revision,
                "version_id": committed_expectation.version_id,
            })),
        );
        Ok(())
    }

    fn timeline_batch_already_published(
        &self,
        project: &Value,
        job_id: &str,
        request: &TimelineRenderBatchRequest,
        cancel: &AtomicBool,
    ) -> ServiceResult<bool> {
        let current = project_expectation(project)?;
        if current.revision != request.base.expected_revision.saturating_add(1) {
            return Ok(false);
        }
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        let expected_reason = match request.base.profile {
            TimelineRenderProfile::Preview => "Published reviewed-timeline preview variations",
            TimelineRenderProfile::Final => "Published reviewed final-render variations",
        };
        let expected_stage = match request.base.profile {
            TimelineRenderProfile::Preview => RevisionStage::Preview,
            TimelineRenderProfile::Final => RevisionStage::FinalRender,
        };
        let Some(record) = manifest.revision_history.last() else {
            return Ok(false);
        };
        if record.revision as i64 != current.revision
            || record.actor != request.base.actor
            || record.reason != expected_reason
            || record.changed_paths != ["/render_artifacts".to_string()]
            || record.invalidated_stages != BTreeSet::from([expected_stage])
        {
            return Ok(false);
        }
        let expected_variations = request
            .variations
            .iter()
            .map(|variation| u64::from(*variation))
            .collect::<BTreeSet<_>>();
        let outputs = project
            .get("outputs")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_store_shape("outputs"))?
            .iter()
            .filter(|output| {
                output.get("version_id").and_then(Value::as_str)
                    == Some(current.version_id.as_str())
                    && output.get("job_id").and_then(Value::as_str) == Some(job_id)
            })
            .collect::<Vec<_>>();
        if outputs.len() != expected_variations.len() {
            return Ok(false);
        }
        let observed_variations = outputs
            .iter()
            .filter_map(|output| {
                output
                    .get("provenance")
                    .and_then(|value| value.get("variation"))
                    .and_then(Value::as_u64)
            })
            .collect::<BTreeSet<_>>();
        if observed_variations != expected_variations {
            return Ok(false);
        }
        let runtime = self.runtime_status(false);
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to recover rendered variations",
        )?;
        for output in outputs {
            self.ensure_not_cancelled(cancel)?;
            let provenance = output.get("provenance").ok_or_else(|| {
                VideoServiceError::new(
                    "video.store_contract_failed",
                    "A recovered variation has no provenance",
                )
            })?;
            if provenance.get("source_version_id").and_then(Value::as_str)
                != Some(request.base.expected_version_id.as_str())
                || provenance.get("manifest_revision").and_then(Value::as_i64)
                    != Some(request.base.expected_revision)
            {
                return Ok(false);
            }
            let variation = provenance
                .get("variation")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid_store_shape("outputs.provenance.variation"))?;
            let expected_kind = if variation > 0 {
                "variation"
            } else if request.base.profile == TimelineRenderProfile::Final {
                "master"
            } else {
                "preview"
            };
            if output.get("kind").and_then(Value::as_str) != Some(expected_kind)
                || output.get("is_primary").and_then(Value::as_bool)
                    != Some(request.base.profile == TimelineRenderProfile::Final && variation == 0)
            {
                return Ok(false);
            }
            let expected_sha = value_string(output, "sha256")?;
            let output_path = self.resolve_absolute_managed_path(&PathBuf::from(value_string(
                output,
                "artifact_path",
            )?))?;
            if sha256_file_with_cancel(&output_path, Some(cancel))? != expected_sha {
                return Err(VideoServiceError::new(
                    "video.integrity_failed",
                    "A recovered variation no longer matches its output checksum",
                ));
            }
            let probe = probe_media(&output_path, ffprobe)?;
            if probe.primary_video_stream.is_none() || probe.duration_us <= 0 {
                return Err(VideoServiceError::new(
                    "video.invalid_render",
                    "A recovered variation is not a playable video",
                ));
            }
            if !manifest
                .render_artifacts
                .iter()
                .any(|artifact| artifact.sha256 == expected_sha)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn run_timeline_render(
        &self,
        job_id: &str,
        request: &TimelineRenderRequest,
        defer_publication: bool,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<Option<PreparedTimelineRender>> {
        let effective_request = effective_timeline_variation_request(request);
        let request = &effective_request;
        self.ensure_not_cancelled(cancel)?;
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let started = Instant::now();
        let expectation = ProjectExpectation {
            revision: request.expected_revision,
            version_id: request.expected_version_id.clone(),
        };
        let reason = match request.profile {
            TimelineRenderProfile::Preview if request.variation > 0 => {
                "Published a reviewed-timeline preview variation"
            }
            TimelineRenderProfile::Preview => "Published a reviewed-timeline preview",
            TimelineRenderProfile::Final if request.variation > 0 => {
                "Published an alternate reviewed final render"
            }
            TimelineRenderProfile::Final => "Published the reviewed final master",
        };
        let invalidated_stage = match request.profile {
            TimelineRenderProfile::Preview => RevisionStage::Preview,
            TimelineRenderProfile::Final => RevisionStage::FinalRender,
        };
        let stage_key = match request.profile {
            TimelineRenderProfile::Preview => "preview_render",
            TimelineRenderProfile::Final => "final_render",
        };
        let scope_key = format!("timeline-variation-{}", request.variation);
        let output_kind = match (request.profile, request.variation) {
            (_, variation) if variation > 0 => "variation",
            (TimelineRenderProfile::Preview, _) => "preview",
            (TimelineRenderProfile::Final, _) => "master",
        };
        let semantic_role = match request.profile {
            TimelineRenderProfile::Preview => "timeline-preview",
            TimelineRenderProfile::Final => "timeline-final",
        };
        let request_sha256 = sha256_bytes(&serde_json::to_vec(request).map_err(json_error)?);
        let project = self.get_project(&request.project_id)?;
        if let Err(conflict) = ensure_project_matches(&project, &expectation) {
            if !defer_publication {
                if let Some(output) = self.recover_single_render_output(
                    &project,
                    job_id,
                    &expectation,
                    &request.actor,
                    reason,
                    invalidated_stage,
                    &request_sha256,
                    semantic_role,
                    request.variation,
                    output_kind,
                    request.profile == TimelineRenderProfile::Final && request.variation == 0,
                    "render_cache_key",
                    Some("caption_cache_key"),
                    cancel,
                )? {
                    let current = project_expectation(&project)?;
                    let provenance = output
                        .get("provenance")
                        .ok_or_else(|| invalid_store_shape("outputs.provenance"))?;
                    let render_key = provenance
                        .get("render_cache_key")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            invalid_store_shape("outputs.provenance.render_cache_key")
                        })?;
                    let output_sha256 = value_string(&output, "sha256")?;
                    self.checkpoint_stage(
                        &request.project_id,
                        Some(&current.version_id),
                        stage_key,
                        &scope_key,
                        job_id,
                        "completed",
                        "light",
                        1.0,
                        render_key,
                        Some(&output_sha256),
                        json!({ "output_id": output.get("id"), "idempotent_replay": true }),
                        None,
                    )?;
                    self.store
                        .update_job(job_id, "running", 0.99)
                        .map_err(VideoServiceError::store)?;
                    emit_progress(
                        callback,
                        job_id,
                        &request.project_id,
                        "published",
                        0.99,
                        "The atomically published timeline render was recovered after restart",
                        Some(output),
                        Some(json!({ "idempotent_replay": true })),
                    );
                    return Ok(None);
                }
            }
            return Err(conflict);
        }
        let manifest: VideoProjectManifest = serde_json::from_value(
            project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        manifest.validate_strict()?;
        validate_timeline_render_contract(&manifest, request.profile)?;
        let render_profile: RenderProfile = request.profile.into();
        validate_media_duration(manifest.timeline_duration_us.0)?;

        let runtime = self.runtime_status(false);
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to assemble the reviewed timeline",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate the assembled timeline",
        )?;
        let resources = render_resource_request(render_profile, runtime.h264_nvenc_runtime);
        let _lease =
            self.acquire_resources(job_id, &request.project_id, resources, cancel, callback)?;
        let options = AssemblyOptions {
            profile: render_profile,
            portrait_layout: request.portrait_layout.clone().into(),
            caption_theme: request.caption_theme,
            include_title_cards: request.include_title_cards,
            include_speaker_cards: request.include_speaker_cards,
            burn_captions: request.burn_captions,
        };

        let source_by_id = manifest
            .source_assets
            .iter()
            .map(|source| (source.id.as_str(), source))
            .collect::<BTreeMap<_, _>>();
        let artifact_by_id = manifest
            .render_artifacts
            .iter()
            .map(|artifact| (artifact.id.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let mut resolved_sources = BTreeMap::new();
        let mut resolved_probes = BTreeMap::<String, RuntimeMediaProbe>::new();
        let mut render_key_builder = CacheKeyBuilder::new(
            match request.profile {
                TimelineRenderProfile::Preview => CacheStage::PreviewRender,
                TimelineRenderProfile::Final => CacheStage::FinalRender,
            },
            format!("{SERVICE_VERSION}:timeline-v2"),
        )
        .tool_version(
            "ffmpeg",
            runtime
                .ffmpeg
                .version
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        );
        for track in &manifest.tracks {
            for clip in &track.clips {
                self.ensure_not_cancelled(cancel)?;
                let (media_id, media_kind, managed_path, expected_sha256) = match (
                    clip.media.source_asset_id.as_deref(),
                    clip.media.render_artifact_id.as_deref(),
                ) {
                    (Some(source_id), None) => {
                        let source = source_by_id.get(source_id).ok_or_else(|| {
                            VideoServiceError::new(
                                "video.source_not_found",
                                "A timeline clip references a missing source",
                            )
                        })?;
                        (
                            source_id,
                            "source",
                            source.managed_path.as_str(),
                            source.sha256.as_str(),
                        )
                    }
                    (None, Some(artifact_id)) => {
                        let artifact = artifact_by_id.get(artifact_id).ok_or_else(|| {
                            VideoServiceError::new(
                                "video.artifact_not_found",
                                "A timeline clip references a missing generated artifact",
                            )
                        })?;
                        (
                            artifact_id,
                            "artifact",
                            artifact.managed_path.as_str(),
                            artifact.sha256.as_str(),
                        )
                    }
                    _ => {
                        return Err(VideoServiceError::new(
                            "video.invalid_timeline_media",
                            "A timeline clip must select exactly one managed media item",
                        ))
                    }
                };
                let resolved_key = format!("{media_kind}:{media_id}");
                if !resolved_sources.contains_key(&resolved_key) {
                    let path = self.resolve_managed_path(managed_path)?;
                    let actual_sha256 = sha256_file(&path)?;
                    if actual_sha256 != expected_sha256 {
                        return Err(VideoServiceError::new(
                            "video.integrity_failed",
                            "Managed timeline media no longer matches its manifest checksum",
                        )
                        .details(json!({ "media_id": media_id, "media_kind": media_kind })));
                    }
                    let probe = probe_media(&path, ffprobe)?;
                    render_key_builder =
                        render_key_builder.artifact(resolved_key.clone(), actual_sha256);
                    resolved_sources.insert(resolved_key.clone(), path);
                    resolved_probes.insert(resolved_key.clone(), probe);
                }
                let probe = resolved_probes.get(&resolved_key).ok_or_else(|| {
                    VideoServiceError::new(
                        "video.store_contract_failed",
                        "Resolved timeline media lost its probe record",
                    )
                })?;
                if matches!(&track.kind, TrackKind::Video) && probe.primary_video_stream.is_none() {
                    return Err(VideoServiceError::new(
                        "video.invalid_timeline_media",
                        "A primary video clip does not contain a video stream",
                    )
                    .details(json!({ "clip_id": clip.id, "media_id": media_id })));
                }
                if matches!(&track.kind, TrackKind::Audio) && probe.primary_audio_stream.is_none() {
                    return Err(VideoServiceError::new(
                        "video.invalid_timeline_media",
                        "An audio timeline clip does not contain an audio stream",
                    )
                    .details(json!({ "clip_id": clip.id, "media_id": media_id })));
                }
            }
        }
        let rendered_visual_ids = manifest
            .visual_layers
            .iter()
            .map(|layer| layer.asset_id.as_str())
            .collect::<BTreeSet<_>>();
        for visual in manifest
            .visual_assets
            .iter()
            .filter(|visual| rendered_visual_ids.contains(visual.id.as_str()))
        {
            self.ensure_not_cancelled(cancel)?;
            let resolved_key = format!("visual:{}", visual.id);
            let path = self.resolve_managed_path(&visual.managed_path)?;
            validate_image_file(&path)?;
            let actual_sha256 = sha256_file(&path)?;
            if actual_sha256 != visual.sha256 {
                return Err(VideoServiceError::new(
                    "video.integrity_failed",
                    "A managed visual asset no longer matches its manifest checksum",
                )
                .details(json!({ "visual_asset_id": visual.id })));
            }
            render_key_builder = render_key_builder.artifact(resolved_key.clone(), actual_sha256);
            resolved_sources.insert(resolved_key, path);
        }

        let caption_key = timeline_caption_cache_key(&manifest, request)?;
        let ass_document = build_ass_document(&manifest, &options)?;
        let expected_ass_sha = sha256_bytes(ass_document.as_bytes());
        let (ass_path, caption_cache_hit) = if let Some(cache) = self
            .store
            .get_video_cache(&caption_key)
            .map_err(VideoServiceError::store)?
        {
            let path = self.resolve_absolute_managed_path(&PathBuf::from(value_string(
                &cache,
                "artifact_path",
            )?))?;
            if sha256_file(&path)? != expected_ass_sha {
                return Err(VideoServiceError::new(
                    "video.cache_collision",
                    "The cached caption document does not match its deterministic input",
                ));
            }
            (path, true)
        } else {
            let path = self.cache_path("captions", &caption_key, "ass")?;
            if !path.is_file() || sha256_file(&path)? != expected_ass_sha {
                write_ass_document_atomic(&path, &ass_document)?;
            }
            secure_managed_file(&path)?;
            self.store
                .put_video_cache(
                    &caption_key,
                    "captions",
                    Some(&request.project_id),
                    &json!({
                        "expected_sha256": expected_ass_sha,
                        "profile": request.profile,
                        "caption_theme": request.caption_theme,
                    }),
                    &path,
                )
                .map_err(VideoServiceError::store)?;
            (path, false)
        };
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "captions_ready",
            0.08,
            "Timeline overlays are ready",
            Some(json!({
                "kind": "captions",
                "artifact_path": ass_path,
                "mime_type": "text/x-ass",
            })),
            Some(json!({ "cache_hit": caption_cache_hit })),
        );

        let rendered_visual_assets = manifest
            .visual_assets
            .iter()
            .filter(|visual| rendered_visual_ids.contains(visual.id.as_str()))
            .map(|visual| {
                json!({
                    "id": visual.id,
                    "sha256": visual.sha256,
                    "mime_type": visual.mime_type,
                    "width": visual.width,
                    "height": visual.height,
                    "has_alpha": visual.has_alpha,
                })
            })
            .collect::<Vec<_>>();
        let render_slice = json!({
            "reviewed_scenes": manifest.reviewed_scenes,
            "tracks": manifest.tracks,
            "gaps": manifest.gaps,
            "captions": manifest.captions,
            "visual_assets": rendered_visual_assets,
            "visual_layers": manifest.visual_layers,
            "layout": manifest.layout,
            "audio_mix": manifest.audio_mix,
            "timeline_duration_us": manifest.timeline_duration_us,
            "frame_rate": manifest.frame_rate,
        });
        let render_profile_value = json!({
            "profile": request.profile,
            "variation": request.variation,
            "caption_theme": request.caption_theme,
            "portrait_layout": request.portrait_layout,
            "include_title_cards": request.include_title_cards,
            "include_speaker_cards": request.include_speaker_cards,
            "burn_captions": request.burn_captions,
        });
        render_key_builder = render_key_builder
            .artifact("ass", expected_ass_sha.clone())
            .manifest_slice(render_slice)
            .profile(render_profile_value.clone());
        let render_key = render_key_builder.build()?.into_string();
        self.checkpoint_stage(
            &request.project_id,
            Some(&expectation.version_id),
            stage_key,
            &scope_key,
            job_id,
            "running",
            resource_class_name(resources.class),
            0.1,
            &render_key,
            None,
            json!({
                "manifest_revision": expectation.revision,
                "manifest_version_id": expectation.version_id,
                "scene_count": manifest.reviewed_scenes.len(),
                "profile": request.profile,
                "variation": request.variation,
            }),
            None,
        )?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "assembling",
            0.1,
            if request.profile == TimelineRenderProfile::Final {
                "Assembling the reviewed final timeline"
            } else {
                "Assembling a fast reviewed-timeline preview"
            },
            None,
            None,
        );

        let namespace = match request.profile {
            TimelineRenderProfile::Preview => "timeline_preview",
            TimelineRenderProfile::Final => "timeline_final",
        };
        let mut render_cache_hit = false;
        let output_path = if let Some(cache) = self
            .store
            .get_video_cache(&render_key)
            .map_err(VideoServiceError::store)?
        {
            render_cache_hit = true;
            PathBuf::from(value_string(&cache, "artifact_path")?)
        } else {
            let output = self.cache_path(namespace, &render_key, "mp4")?;
            if !output.is_file() {
                let _storage_lease = self.reserve_storage(
                    format!("{job_id}:timeline:{render_key}"),
                    &self.video_root,
                    with_disk_headroom(
                        estimated_render_bytes(manifest.timeline_duration_us.0, render_profile)?,
                        2,
                    ),
                    "timeline_render",
                )?;
                let staging = sibling_staging_path(&output)?;
                let plan = build_timeline_render_plan(
                    ffmpeg,
                    &manifest,
                    &resolved_sources,
                    request.burn_captions.then_some(ass_path.as_path()),
                    &staging,
                    &options,
                    runtime.h264_nvenc_runtime,
                )?;
                self.execute_render_plan(
                    job_id,
                    &request.project_id,
                    &plan,
                    Some(manifest.timeline_duration_us.0),
                    0.1,
                    0.9,
                    cancel,
                    callback,
                )?;
                publish_atomic(&staging, &output, |path| {
                    let probe = probe_media(path, ffprobe)?;
                    if probe.primary_video_stream.is_none()
                        || probe.primary_audio_stream.is_none()
                        || probe.duration_us <= 0
                    {
                        return Err(MediaError::new(
                            "invalid_timeline_render",
                            "The assembled timeline is not a playable audio/video MP4",
                        ));
                    }
                    let tolerance_us = 300_000_i64.max(
                        2_000_000_i64.saturating_mul(i64::from(manifest.frame_rate.denominator))
                            / i64::from(manifest.frame_rate.numerator),
                    );
                    if (probe.duration_us - manifest.timeline_duration_us.0).abs() > tolerance_us {
                        return Err(MediaError::new(
                            "timeline_duration_mismatch",
                            "The assembled timeline duration does not match the manifest clock",
                        ));
                    }
                    Ok(())
                })?;
            } else {
                let probe = probe_media(&output, ffprobe)?;
                if probe.primary_video_stream.is_none() || probe.primary_audio_stream.is_none() {
                    return Err(VideoServiceError::new(
                        "video.cache_invalid",
                        "The existing timeline cache entry is not playable",
                    ));
                }
            }
            self.store
                .put_video_cache(
                    &render_key,
                    namespace,
                    Some(&request.project_id),
                    &json!({
                        "manifest_revision": expectation.revision,
                        "profile": render_profile_value,
                        "caption_cache_key": caption_key,
                    }),
                    &output,
                )
                .map_err(VideoServiceError::store)?;
            output
        };
        let output_path = self.resolve_absolute_managed_path(&output_path)?;
        self.ensure_not_cancelled(cancel)?;
        // Re-check before any manifest/output publication. A stale render may
        // remain reusable in content-addressed cache, but never becomes current.
        ensure_project_matches(&self.get_project(&request.project_id)?, &expectation)?;
        let output_probe = probe_media(&output_path, ffprobe)?;
        let output_sha = sha256_file(&output_path)?;
        let stream = output_probe.primary_video_stream.and_then(|index| {
            output_probe
                .streams
                .iter()
                .find(|stream| stream.index == index)
        });
        let (width, height) = stream
            .and_then(|stream| stream.width.zip(stream.height))
            .ok_or_else(|| {
                VideoServiceError::new(
                    "video.invalid_render",
                    "The assembled timeline has no video dimensions",
                )
            })?;
        let caption_artifact = RenderArtifact {
            id: new_id(),
            role: RenderArtifactRole::Captions,
            scene_id: None,
            managed_path: self.relative_managed_path(&ass_path)?,
            sha256: expected_ass_sha.clone(),
            cache_key: caption_key.clone(),
            mime_type: "text/x-ass".to_string(),
            duration_us: None,
            width: None,
            height: None,
            publication_state: PublicationState::Published,
            created_at: utc_now(),
        };
        caption_artifact.validate()?;
        let render_artifact = RenderArtifact {
            id: new_id(),
            role: match request.profile {
                TimelineRenderProfile::Preview => RenderArtifactRole::Preview,
                TimelineRenderProfile::Final => RenderArtifactRole::FinalMaster,
            },
            scene_id: None,
            managed_path: self.relative_managed_path(&output_path)?,
            sha256: output_sha.clone(),
            cache_key: render_key.clone(),
            mime_type: "video/mp4".to_string(),
            duration_us: Some(Microseconds(output_probe.duration_us)),
            width: Some(width),
            height: Some(height),
            publication_state: PublicationState::Published,
            created_at: utc_now(),
        };
        render_artifact.validate()?;
        if defer_publication {
            return Ok(Some(PreparedTimelineRender {
                request: request.clone(),
                caption_artifact,
                render_artifact,
                output_path,
                output_sha,
                output_duration_us: output_probe.duration_us,
                width,
                height,
                caption_key,
                render_key,
                stage_key: stage_key.to_string(),
                scope_key,
                resource_class: resource_class_name(resources.class).to_string(),
                caption_cache_hit,
                render_cache_hit,
                wall_seconds: started.elapsed().as_secs_f64(),
                scene_count: manifest.reviewed_scenes.len(),
                nvenc_with_software_fallback: runtime.h264_nvenc_runtime,
            }));
        }
        let has_caption_artifact = manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.cache_key == caption_key);
        let has_render_artifact = manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.cache_key == render_key);
        let adds_render_artifacts = !has_caption_artifact || !has_render_artifact;
        let target_revision = if adds_render_artifacts {
            expectation.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The timeline output version could not be advanced",
                )
            })?
        } else {
            expectation.revision
        };
        let output_id = stable_output_id(
            &request.project_id,
            target_revision,
            semantic_role,
            &render_key,
            request.variation,
        );
        let build_output_request = |version_id: Option<&str>| {
            json!({
                "id": output_id,
                "project_id": request.project_id,
                "version_id": version_id,
                "job_id": job_id,
                "kind": output_kind,
                "label": match (request.profile, request.variation) {
                    (TimelineRenderProfile::Final, 0) => "Final master".to_string(),
                    (TimelineRenderProfile::Final, variation) => format!("Final variation {variation}"),
                    (TimelineRenderProfile::Preview, 0) => "Timeline preview".to_string(),
                    (TimelineRenderProfile::Preview, variation) => format!("Preview variation {variation}"),
                },
                "artifact_path": output_path,
                "mime_type": "video/mp4",
                "sha256": output_sha,
                "duration_us": output_probe.duration_us,
                "width": width,
                "height": height,
                "is_primary": request.profile == TimelineRenderProfile::Final && request.variation == 0,
                "provenance": {
                    "producer": "soundAr Video Studio timeline assembler",
                    "producer_version": SERVICE_VERSION,
                    "manifest_revision": expectation.revision,
                    "source_version_id": expectation.version_id,
                    "request_sha256": request_sha256,
                    "render_cache_key": render_key,
                    "caption_cache_key": caption_key,
                    "profile": request.profile,
                    "variation": request.variation,
                    "caption_theme": request.caption_theme,
                    "portrait_layout": request.portrait_layout,
                    "nvenc_with_software_fallback": runtime.h264_nvenc_runtime,
                },
            })
        };
        let publication_lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(&current, &expectation)?;
        #[cfg(test)]
        if adds_render_artifacts {
            self.trigger_single_render_test_failpoint(
                SingleRenderTestFailpoint::TimelineBeforeAtomicPublication,
            )?;
        }
        let (committed, output) = if adds_render_artifacts {
            let actor = require_text(
                &request.actor,
                "video.invalid_actor",
                "An actor is required",
            )?;
            let mut next_manifest = manifest.clone();
            if i64::try_from(next_manifest.revision).ok() != Some(expectation.revision) {
                return Err(VideoServiceError::new(
                    "video.revision_integrity_failed",
                    "The frozen timeline manifest and project revision are not aligned",
                ));
            }
            if !has_caption_artifact {
                next_manifest.render_artifacts.push(caption_artifact);
            }
            if !has_render_artifact {
                next_manifest.render_artifacts.push(render_artifact);
            }
            let next_revision = next_manifest.revision.checked_add(1).ok_or_else(|| {
                VideoServiceError::new(
                    "video.revision_overflow",
                    "The timeline artifact revision could not be advanced",
                )
            })?;
            let created_at = utc_now();
            let parent_id = next_manifest
                .revision_history
                .last()
                .map(|record| record.id.clone());
            next_manifest.revision = next_revision;
            next_manifest.updated_at = created_at.clone();
            next_manifest.revision_history.push(RevisionRecord {
                id: new_id(),
                revision: next_revision,
                parent_id,
                actor: actor.to_string(),
                reason: reason.to_string(),
                changed_paths: vec!["/render_artifacts".to_string()],
                invalidated_stages: BTreeSet::from([invalidated_stage]),
                created_at,
            });
            next_manifest.validate_strict()?;
            let output_request = build_output_request(None);
            let committed = self
                .store
                .commit_video_manifest_with_outputs_cancellable(
                    &request.project_id,
                    expectation.revision,
                    &serde_json::to_value(&next_manifest).map_err(json_error)?,
                    actor,
                    reason,
                    &publication_lock.token,
                    Some(match request.profile {
                        TimelineRenderProfile::Preview => "ready",
                        TimelineRenderProfile::Final => "completed",
                    }),
                    &[output_request],
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            let output = committed
                .get("outputs")
                .and_then(Value::as_array)
                .and_then(|outputs| {
                    outputs.iter().find(|output| {
                        output.get("id").and_then(Value::as_str) == Some(output_id.as_str())
                    })
                })
                .cloned()
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.store_contract_failed",
                        "The atomically published timeline output could not be reloaded",
                    )
                })?;
            (committed, output)
        } else {
            let output_request = build_output_request(Some(&expectation.version_id));
            let output = self
                .store
                .publish_video_output_current_cancellable(
                    &output_request,
                    expectation.revision,
                    &expectation.version_id,
                    &publication_lock.token,
                    cancel,
                )
                .map_err(VideoServiceError::store)?;
            (current, output)
        };
        #[cfg(test)]
        self.trigger_single_render_test_failpoint(
            SingleRenderTestFailpoint::TimelineAfterAtomicPublication,
        )?;
        let committed_expectation = project_expectation(&committed)?;
        ensure_project_matches(
            &self.get_project(&request.project_id)?,
            &committed_expectation,
        )?;
        drop(publication_lock);
        self.checkpoint_stage(
            &request.project_id,
            Some(&committed_expectation.version_id),
            stage_key,
            &scope_key,
            job_id,
            "completed",
            resource_class_name(resources.class),
            1.0,
            &render_key,
            Some(&output_sha),
            json!({
                "output_id": output.get("id"),
                "render_cache_hit": render_cache_hit,
                "caption_cache_hit": caption_cache_hit,
            }),
            None,
        )?;
        let wall_seconds = started.elapsed().as_secs_f64();
        let media_seconds = output_probe.duration_us as f64 / 1_000_000.0;
        let _ = self.store.record_video_performance(&json!({
            "project_id": request.project_id,
            "job_id": job_id,
            "operation": if request.profile == TimelineRenderProfile::Final { "timeline_final_render" } else { "timeline_preview_render" },
            "profile": if request.profile == TimelineRenderProfile::Final { "final" } else { "preview" },
            "wall_seconds": wall_seconds,
            "media_seconds": media_seconds,
            "realtime_factor": if media_seconds > 0.0 { Some(wall_seconds / media_seconds) } else { None },
            "cache_hit": render_cache_hit,
            "details": {
                "caption_cache_hit": caption_cache_hit,
                "scene_count": manifest.reviewed_scenes.len(),
                "variation": request.variation,
                "encoder": if runtime.h264_nvenc_runtime { "nvenc_with_software_fallback" } else { "libx264" },
            },
        }));
        self.store
            .update_job(job_id, "running", 0.99)
            .map_err(VideoServiceError::store)?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "published",
            0.99,
            if request.profile == TimelineRenderProfile::Final {
                "The assembled final master is ready to play"
            } else {
                "The assembled timeline preview is ready to play"
            },
            Some(output),
            Some(json!({
                "render_cache_hit": render_cache_hit,
                "caption_cache_hit": caption_cache_hit,
                "wall_seconds": wall_seconds,
            })),
        );
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_audio_portrait_plan(
        &self,
        ffmpeg: &Path,
        audio: &Path,
        output: &Path,
        profile: RenderProfile,
        h264_nvenc_runtime: bool,
        title: &str,
        variation: u16,
    ) -> ServiceResult<RenderCommandPlan> {
        let ffmpeg = fs::canonicalize(ffmpeg).map_err(|error| {
            VideoServiceError::io(
                "video.ffmpeg_unavailable",
                "FFmpeg could not be resolved",
                error,
            )
        })?;
        let audio = fs::canonicalize(audio).map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The audio source could not be resolved",
                error,
            )
        })?;
        let parent = output.parent().ok_or_else(|| {
            VideoServiceError::new(
                "video.invalid_render_output",
                "The render output has no parent directory",
            )
        })?;
        self.secure_managed_directory(parent)?;
        if output.exists() {
            return Err(VideoServiceError::new(
                "video.render_output_exists",
                "The render staging output already exists",
            ));
        }
        let title_key = sha256_bytes(title.as_bytes());
        let title_dir = self.video_root.join("cache").join("title");
        self.secure_managed_directory(&title_dir)?;
        let title_path = title_dir.join(format!("{title_key}.txt"));
        if !title_path.is_file() {
            let staging = sibling_staging_path(&title_path)?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(PRIVATE_FILE_MODE)
                .open(&staging)
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.title_card_failed",
                        "The title card could not be staged",
                        error,
                    )
                })?;
            file.write_all(truncate_chars(title, 160).as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.title_card_failed",
                        "The title card could not be written",
                        error,
                    )
                })?;
            secure_managed_file(&staging)?;
            publish_atomic(&staging, &title_path, |path| {
                let valid = fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0);
                if valid {
                    Ok(())
                } else {
                    Err(MediaError::new(
                        "invalid_title_card",
                        "The title card is empty",
                    ))
                }
            })?;
        }
        secure_managed_file(&title_path)?;
        let (width, height) = portrait_dimensions(profile);
        let (background, foreground) = match variation % 3 {
            1 => ("0xe9e9e7", "0x262626"),
            2 => ("0xf7f7f6", "0x3f3f46"),
            _ => ("0xf2f2f0", "0x303030"),
        };
        let waveform_width = width.saturating_sub(width / 7);
        let waveform_height = height / 5;
        let title_path_filter = escape_filter_path(&title_path);
        let filter = format!(
            "[1:a:0]aformat=sample_fmts=fltp:channel_layouts=stereo,showwaves=s={waveform_width}x{waveform_height}:mode=cline:draw=full:colors={foreground},format=rgba,colorkey=0x000000:0.10:0.0[wave];[0:v:0][wave]overlay=(W-w)/2:(H-h)/2[base];[base]drawtext=textfile='{title_path_filter}':expansion=none:fontcolor={foreground}:fontsize={}:line_spacing=12:x=(w-text_w)/2:y=h*0.18[v]",
            (width / 18).clamp(32, 64)
        );
        let duration_seconds = format!(
            "{:.6}",
            probe_duration_seconds(&audio, &self.runtime_status(false))?
        );
        let mut common = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-nostdin"),
            OsString::from("-n"),
            OsString::from("-progress"),
            OsString::from("pipe:2"),
            OsString::from("-stats_period"),
            OsString::from("0.25"),
            OsString::from("-f"),
            OsString::from("lavfi"),
            OsString::from("-i"),
            OsString::from(format!("color=c={background}:s={width}x{height}:r=30")),
        ];
        common.extend(local_media_input_args(&audio)?);
        common.extend([
            OsString::from("-filter_complex"),
            OsString::from(filter),
            OsString::from("-map"),
            OsString::from("[v]"),
            OsString::from("-map"),
            OsString::from("1:a:0"),
            OsString::from("-t"),
            OsString::from(duration_seconds),
        ]);
        let make = |encoder: VideoEncoder| {
            let mut args = common.clone();
            args.extend(service_encoder_arguments(encoder, profile));
            args.extend([
                OsString::from("-c:a"),
                OsString::from("aac"),
                OsString::from("-b:a"),
                OsString::from(if profile == RenderProfile::Final {
                    "192k"
                } else {
                    "128k"
                }),
                OsString::from("-ar"),
                OsString::from("48000"),
                OsString::from("-movflags"),
                OsString::from("+faststart"),
                OsString::from("-f"),
                OsString::from("mp4"),
                output.as_os_str().to_os_string(),
            ]);
            RenderCommand {
                program: ffmpeg.clone(),
                args,
                output: output.to_path_buf(),
                encoder,
                emits_progress: true,
            }
        };
        let workload_class = if profile == RenderProfile::Final {
            RenderWorkloadClass::Heavy
        } else {
            RenderWorkloadClass::Medium
        };
        Ok(if h264_nvenc_runtime {
            RenderCommandPlan {
                profile,
                workload_class,
                primary: make(VideoEncoder::H264Nvenc),
                software_fallback: Some(make(VideoEncoder::Libx264)),
            }
        } else {
            RenderCommandPlan {
                profile,
                workload_class,
                primary: make(VideoEncoder::Libx264),
                software_fallback: None,
            }
        })
    }
}

impl VideoStudioService {
    fn perform_local_import(
        &self,
        job_id: &str,
        request: &DurableLocalImportRequest,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        self.ensure_not_cancelled(cancel)?;
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let imported_at = self.durable_job_created_at(job_id)?;
        let started = Instant::now();
        let runtime = self.runtime_status(false);
        let source_metadata = fs::symlink_metadata(&request.source_path).map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected local media could not be re-inspected",
                error,
            )
        })?;
        if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
            return Err(VideoServiceError::new(
                "video.invalid_source",
                "The selected media must remain a regular local file",
            ));
        }
        let initial_identity = LocalSourceIdentity::from_metadata(&source_metadata);
        self.verify_local_import_origin(&request.source_path, &request.origin)?;
        validate_source_size(source_metadata.len())?;
        ensure_disk_capacity(
            &self.video_root,
            with_disk_headroom(source_metadata.len(), 3),
            "local_import",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to import local media",
        )?;
        required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to prepare local media",
        )?;
        let input_key = sha256_bytes(request.source_path.to_string_lossy().as_bytes());
        self.checkpoint_stage(
            &request.project_id,
            None,
            "ingest",
            "source",
            job_id,
            "running",
            "medium",
            0.03,
            &input_key,
            None,
            json!({ "source": "local" }),
            None,
        )?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "validating",
            0.03,
            "Validating local media",
            None,
            None,
        );
        let initial_probe = probe_media(&request.source_path, ffprobe)?;
        validate_media_duration(initial_probe.duration_us)?;
        let request_resources =
            if initial_probe.primary_video_stream.is_some() && runtime.h264_nvenc_runtime {
                ResourceRequest::medium_nvenc()
            } else if initial_probe.primary_video_stream.is_some() {
                ResourceRequest {
                    class: ResourceClass::Medium,
                    vram_mb: 0,
                    cpu_threads: 6,
                    io_slots: 2,
                    nvenc_sessions: 0,
                }
            } else {
                ResourceRequest::light()
            };
        let _lease = self.acquire_resources(
            job_id,
            &request.project_id,
            request_resources,
            cancel,
            callback,
        )?;
        // Admission may wait behind another media workload. Open once with
        // O_NOFOLLOW, bind the copy to that descriptor, and require the exact
        // inode/size/mtime that FFprobe validated before admission.
        let source_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&request.source_path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.source_not_found",
                    "The selected local media could not be opened safely after admission",
                    error,
                )
            })?;
        let admitted_metadata = source_file.metadata().map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected local media could not be re-inspected after admission",
                error,
            )
        })?;
        if !admitted_metadata.is_file()
            || LocalSourceIdentity::from_metadata(&admitted_metadata) != initial_identity
        {
            return Err(VideoServiceError::new(
                "video.source_changed",
                "The selected local media changed after validation; choose it again",
            ));
        }
        validate_source_size(admitted_metadata.len())?;
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:local-import"),
            &self.video_root,
            // The descriptor-bound managed copy can consume at most the full
            // 8 GiB source cap. Proxy/thumbnail/waveform builders acquire
            // their own reservations, so do not double-count their headroom
            // in this copy lease.
            with_disk_headroom(MAX_SOURCE_BYTES, 1),
            "local_import",
        )?;
        let asset_id =
            stable_import_asset_id(&request.project_id, job_id, "source", "local-primary");
        let managed = self.prepare_local_source(
            job_id,
            &request.project_id,
            &asset_id,
            &request.source_path,
            source_file,
            initial_identity,
            initial_probe,
            cancel,
            callback,
        )?;
        let (source_kind, provenance_kind, producer, mut provenance_metadata) =
            match &request.origin {
                DurableLocalImportOrigin::UserUpload => (
                    if managed.probe.primary_video_stream.is_some() {
                        SourceAssetKind::LocalVideo
                    } else {
                        SourceAssetKind::LocalAudio
                    },
                    ProvenanceKind::UserUpload,
                    "soundAr Video Studio".to_string(),
                    BTreeMap::new(),
                ),
                DurableLocalImportOrigin::SoundArHistory {
                    history_id,
                    generation_job_id,
                    generation_kind,
                    model_id,
                    voice,
                    engine,
                } => {
                    if managed.probe.primary_video_stream.is_some()
                        || managed.probe.primary_audio_stream.is_none()
                    {
                        return Err(VideoServiceError::new(
                            "video.invalid_soundar_origin",
                            "A registered soundAr History artifact must be audio-only",
                        ));
                    }
                    (
                        if generation_kind == "music" {
                            SourceAssetKind::SoundArMusic
                        } else {
                            SourceAssetKind::SoundArSpeech
                        },
                        ProvenanceKind::GeneratedLocally,
                        format!("soundAr {engine}"),
                        BTreeMap::from([
                            ("history_id".to_string(), Value::String(history_id.clone())),
                            (
                                "generation_job_id".to_string(),
                                Value::String(generation_job_id.clone()),
                            ),
                            (
                                "generation_kind".to_string(),
                                Value::String(generation_kind.clone()),
                            ),
                            ("model_id".to_string(), Value::String(model_id.clone())),
                            ("voice".to_string(), Value::String(voice.clone())),
                            ("engine".to_string(), Value::String(engine.clone())),
                        ]),
                    )
                }
            };
        provenance_metadata.insert("source_clock_preserved".to_string(), Value::Bool(true));
        provenance_metadata.insert(
            "display_title".to_string(),
            Value::String(
                request
                    .title
                    .clone()
                    .unwrap_or_else(|| display_name(&request.source_path)),
            ),
        );
        if let Some(parent_job_id) = request.parent_job_id.as_deref() {
            provenance_metadata.insert(
                "video_parent_job_id".to_string(),
                Value::String(parent_job_id.to_string()),
            );
            provenance_metadata.insert(
                "video_project_id".to_string(),
                Value::String(request.project_id.clone()),
            );
            provenance_metadata.insert(
                "purpose".to_string(),
                Value::String("prompt_to_video".to_string()),
            );
        }
        let source = ManagedSource {
            id: asset_id,
            path: managed.path,
            relative_path: managed.relative_path,
            sha256: managed.sha256,
            probe: managed.probe,
            kind: source_kind,
            provenance: Provenance {
                kind: provenance_kind,
                original_uri: None,
                imported_at,
                producer,
                producer_version: Some(SERVICE_VERSION.to_string()),
                metadata: provenance_metadata,
            },
            rights: None,
        };
        self.ingest_managed_source(
            job_id,
            &request.project_id,
            &request.actor,
            source,
            &runtime,
            request.parent_job_id.as_deref(),
            cancel,
            callback,
        )?;
        let wall_seconds = started.elapsed().as_secs_f64();
        let media_seconds = managed_duration_seconds_from_project(self, &request.project_id).ok();
        let _ = self.store.record_video_performance(&json!({
            "project_id": request.project_id,
            "job_id": job_id,
            "operation": "local_import",
            "profile": "proxy",
            "wall_seconds": wall_seconds,
            "media_seconds": media_seconds,
            "realtime_factor": media_seconds.filter(|value| *value > 0.0).map(|value| wall_seconds / value),
            "cache_hit": false,
            "details": { "source_kind": "local" },
        }));
        Ok(())
    }

    fn perform_link_import(
        &self,
        job_id: &str,
        request: &LinkImportRequest,
        canonical_url: &str,
        rights: RightsConfirmation,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<()> {
        self.ensure_not_cancelled(cancel)?;
        let status = self
            .store
            .start_job(job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let imported_at = self.durable_job_created_at(job_id)?;
        let started = Instant::now();
        let runtime = self.runtime_status(false);
        let validated = validate_import_url(canonical_url)?;
        let requested = validate_import_url(&request.url)?;
        if validated.is_playlist
            || validated.canonical != canonical_url
            || requested.canonical != canonical_url
            || rights.source_uri != canonical_url
        {
            return Err(VideoServiceError::new(
                "video.link_request_changed",
                "The authorized link changed before download and must be confirmed again",
            ));
        }
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:link-import"),
            &self.video_root,
            with_disk_headroom(MAX_SOURCE_BYTES, 3),
            "link_import",
        )?;
        let yt_dlp = required_tool_path(
            &runtime.yt_dlp,
            "video.yt_dlp_unavailable",
            "yt-dlp is required to import this link",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate imported media",
        )?;
        let request_resources = if runtime.h264_nvenc_runtime {
            ResourceRequest::medium_nvenc()
        } else {
            ResourceRequest {
                class: ResourceClass::Medium,
                vram_mb: 0,
                cpu_threads: 6,
                io_slots: 3,
                nvenc_sessions: 0,
            }
        };
        let _lease = self.acquire_resources(
            job_id,
            &request.project_id,
            request_resources,
            cancel,
            callback,
        )?;
        // Shared GPU/IO admission can queue for an arbitrary time. Reserve no
        // assumptions from intake: require the conservative download budget
        // again immediately before starting yt-dlp.
        ensure_disk_capacity(
            &self.video_root,
            with_disk_headroom(MAX_SOURCE_BYTES, 3),
            "link_import",
        )?;
        let input_key = sha256_bytes(canonical_url.as_bytes());
        self.checkpoint_stage(
            &request.project_id,
            None,
            "ingest",
            "source",
            job_id,
            "running",
            "medium",
            0.03,
            &input_key,
            None,
            json!({ "source": "authorized_link", "canonical_url": canonical_url, "rights_receipt_id": rights.id }),
            None,
        )?;
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "downloading",
            0.05,
            "Downloading one authorized source",
            None,
            None,
        );
        let asset_id = stable_import_asset_id(
            &request.project_id,
            job_id,
            "source",
            "authorized-link-primary",
        );
        let source_dir = self.project_dir(&request.project_id)?.join("sources");
        self.secure_managed_directory(&source_dir)?;
        let prefix = format!(".download-{}", new_id());
        let output_template = source_dir.join(format!("{prefix}.%(ext)s"));
        preflight_import_url_destination(&validated)?;
        let proxy = PublicHttpsProxy::start()?;
        let args = yt_dlp_single_source_download_args(&output_template, canonical_url, proxy.url());
        let output_quota = CommandOutputQuota {
            directory: source_dir.clone(),
            prefix: prefix.clone(),
            max_file_bytes: MAX_SOURCE_BYTES,
            max_aggregate_bytes: MAX_SOURCE_BYTES,
        };
        let captured = run_captured_command(
            yt_dlp,
            &args,
            LINK_DOWNLOAD_TIMEOUT,
            Some(cancel),
            MAX_CAPTURE_BYTES,
            Some(proxy.url()),
            Some(&output_quota),
        );
        if let Err(error) = &captured {
            cleanup_prefixed_files(&source_dir, &prefix);
            return Err(error.clone());
        }
        let captured = captured?;
        if !captured.status.success() {
            cleanup_prefixed_files(&source_dir, &prefix);
            return Err(command_failed_error(
                "video.link_import_failed",
                "The authorized source could not be downloaded",
                &captured,
                true,
            ));
        }
        let mut published_path = None;
        let prepared = (|| -> ServiceResult<(PathBuf, String, String, RuntimeMediaProbe)> {
            self.ensure_not_cancelled(cancel)?;
            let downloaded = single_downloaded_file(&source_dir, &prefix)?;
            let downloaded_size = fs::metadata(&downloaded)
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.link_import_failed",
                        "The downloaded source could not be inspected",
                        error,
                    )
                })?
                .len();
            validate_source_size(downloaded_size)?;
            secure_managed_file(&downloaded)?;
            let probe = probe_media(&downloaded, ffprobe)?;
            validate_media_duration(probe.duration_us)?;
            if probe.primary_video_stream.is_none() && probe.primary_audio_stream.is_none() {
                return Err(VideoServiceError::new(
                    "video.invalid_source",
                    "The imported source contains no playable audio or video",
                ));
            }
            let extension = safe_extension(&downloaded, probe.primary_video_stream.is_some());
            let final_path = source_dir.join(format!("source-{asset_id}.{extension}"));
            let staged_checksum = sha256_file_with_cancel(&downloaded, Some(cancel))?;
            let (managed_path, newly_published) = self.publish_import_staging_once(
                &request.project_id,
                &asset_id,
                &downloaded,
                &final_path,
                &staged_checksum,
                cancel,
                |path| probe_media(path, ffprobe).map(|_| ()),
            )?;
            if newly_published {
                published_path = Some(managed_path.clone());
            }
            self.ensure_not_cancelled(cancel)?;
            let checksum = sha256_file_with_cancel(&managed_path, Some(cancel))?;
            let final_probe = probe_media(&managed_path, ffprobe)?;
            validate_source_size(final_probe.size_bytes)?;
            validate_media_duration(final_probe.duration_us)?;
            let relative_path = self.relative_managed_path(&managed_path)?;
            Ok((managed_path, relative_path, checksum, final_probe))
        })();
        let (source_path, relative_path, checksum, final_probe) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                cleanup_failed_download(&source_dir, &prefix, published_path.as_deref());
                return Err(error);
            }
        };
        cleanup_prefixed_files(&source_dir, &prefix);
        emit_progress(
            callback,
            job_id,
            &request.project_id,
            "downloaded",
            0.25,
            "Authorized source is ready",
            Some(json!({ "path": source_path, "kind": "source" })),
            None,
        );
        let source = ManagedSource {
            id: asset_id,
            path: source_path,
            relative_path,
            sha256: checksum,
            probe: final_probe,
            kind: SourceAssetKind::ImportedLink,
            provenance: Provenance {
                kind: ProvenanceKind::AuthorizedLink,
                original_uri: Some(canonical_url.to_string()),
                imported_at,
                producer: "soundAr Video Studio + yt-dlp".to_string(),
                producer_version: runtime.yt_dlp.version.clone(),
                metadata: BTreeMap::from([
                    ("source_clock_preserved".to_string(), Value::Bool(true)),
                    ("single_source_only".to_string(), Value::Bool(true)),
                    (
                        "display_title".to_string(),
                        Value::String(
                            request
                                .title
                                .clone()
                                .unwrap_or_else(|| "Imported source".to_string()),
                        ),
                    ),
                ]),
            },
            rights: Some(rights),
        };
        self.ingest_managed_source(
            job_id,
            &request.project_id,
            &request.actor,
            source,
            &runtime,
            None,
            cancel,
            callback,
        )?;
        let wall_seconds = started.elapsed().as_secs_f64();
        let _ = self.store.record_video_performance(&json!({
            "project_id": request.project_id,
            "job_id": job_id,
            "operation": "link_import",
            "profile": "single_source",
            "wall_seconds": wall_seconds,
            "cache_hit": false,
            "details": { "canonical_url_sha256": input_key },
        }));
        Ok(())
    }

    /// Publishes one deterministic import source without ever replacing an
    /// existing inode. Store/manifest identity wins over a newly inferred
    /// extension, and crash leftovers are adopted only when their bytes match
    /// the newly staged source exactly.
    #[allow(clippy::too_many_arguments)]
    fn publish_import_staging_once<F>(
        &self,
        project_id: &str,
        asset_id: &str,
        staging: &Path,
        suggested_final_path: &Path,
        staged_sha256: &str,
        cancel: &AtomicBool,
        validate: F,
    ) -> ServiceResult<(PathBuf, bool)>
    where
        F: Fn(&Path) -> Result<(), MediaError>,
    {
        let result = (|| -> ServiceResult<(PathBuf, bool)> {
            self.ensure_not_cancelled(cancel)?;
            validate_hash(staged_sha256)?;
            let source_dir = self.project_dir(project_id)?.join("sources");
            self.secure_managed_directory(&source_dir)?;
            let source_dir = fs::canonicalize(&source_dir).map_err(|error| {
                VideoServiceError::io(
                    "video.storage_unavailable",
                    "The managed source directory could not be resolved",
                    error,
                )
            })?;
            if suggested_final_path.parent() != Some(source_dir.as_path()) {
                return Err(VideoServiceError::new(
                    "video.unsafe_artifact_path",
                    "A deterministic import target escaped its managed source directory",
                ));
            }
            let stable_prefix = format!("source-{asset_id}.");
            let project = self.get_project(project_id)?;
            let manifest: VideoProjectManifest = serde_json::from_value(
                project
                    .get("manifest")
                    .cloned()
                    .ok_or_else(|| invalid_store_shape("manifest"))?,
            )
            .map_err(json_error)?;
            manifest.validate_strict()?;
            let manifest_source = manifest
                .source_assets
                .iter()
                .find(|source| source.id == asset_id);
            let asset_row = project
                .get("assets")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_store_shape("assets"))?
                .iter()
                .find(|asset| asset.get("id").and_then(Value::as_str) == Some(asset_id));

            let inspect_bound =
                |path: &Path, expected_sha256: Option<&str>| -> ServiceResult<(PathBuf, bool)> {
                    let path = self.resolve_absolute_managed_path(path)?;
                    if path.parent() != Some(source_dir.as_path())
                        || !path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(&stable_prefix))
                    {
                        return Err(VideoServiceError::new(
                        "video.import_identity_conflict",
                        "The durable import row points outside its exact managed source identity",
                    ));
                    }
                    let actual_sha256 = sha256_file_with_cancel(&path, Some(cancel))?;
                    if expected_sha256.is_some_and(|expected| expected != actual_sha256)
                        || actual_sha256 != staged_sha256
                    {
                        return Err(VideoServiceError::new(
                            "video.import_identity_conflict",
                            "This durable import already owns different managed source bytes",
                        )
                        .details(json!({
                            "managed_sha256": actual_sha256,
                            "staged_sha256": staged_sha256,
                        })));
                    }
                    validate(&path)?;
                    fs::remove_file(staging).map_err(|error| {
                        VideoServiceError::io(
                            "video.import_cleanup_failed",
                            "The duplicate import staging file could not be removed",
                            error,
                        )
                    })?;
                    Ok((path, false))
                };

            if let Some(asset_row) = asset_row {
                let row_sha256 = value_string(asset_row, "content_sha256")?;
                validate_hash(&row_sha256)?;
                let row_path = PathBuf::from(value_string(asset_row, "local_path")?);
                if let Some(manifest_source) = manifest_source {
                    if manifest_source.sha256 != row_sha256
                        || self.resolve_managed_path(&manifest_source.managed_path)?
                            != self.resolve_absolute_managed_path(&row_path)?
                    {
                        return Err(VideoServiceError::new(
                            "video.integrity_failed",
                            "The durable import manifest and media row disagree",
                        ));
                    }
                }
                return inspect_bound(&row_path, Some(&row_sha256));
            }
            if manifest_source.is_some() {
                return Err(VideoServiceError::new(
                    "video.integrity_failed",
                    "A manifest source has no durable media row",
                ));
            }

            // A process may have stopped after the no-replace rename but
            // before its pending row was written. Search by stable asset id so
            // a changed container extension cannot create a second target.
            let mut crash_candidates = Vec::new();
            for entry in fs::read_dir(&source_dir).map_err(|error| {
                VideoServiceError::io(
                    "video.storage_unavailable",
                    "Managed source storage could not be inspected",
                    error,
                )
            })? {
                let entry = entry.map_err(|error| {
                    VideoServiceError::io(
                        "video.storage_unavailable",
                        "A managed source entry could not be inspected",
                        error,
                    )
                })?;
                let name = entry.file_name();
                if !name
                    .to_str()
                    .is_some_and(|name| name.starts_with(&stable_prefix))
                {
                    continue;
                }
                let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                    VideoServiceError::io(
                        "video.storage_unavailable",
                        "A deterministic managed source could not be inspected",
                        error,
                    )
                })?;
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(VideoServiceError::new(
                        "video.unsafe_storage_path",
                        "A deterministic managed source target is not a regular file",
                    ));
                }
                crash_candidates.push(entry.path());
            }
            if crash_candidates.len() > 1 {
                return Err(VideoServiceError::new(
                    "video.import_identity_conflict",
                    "A durable import identity has multiple managed source targets",
                ));
            }
            if let Some(path) = crash_candidates.first() {
                return inspect_bound(path, None);
            }

            validate(staging)?;
            File::open(staging)
                .and_then(|file| file.sync_all())
                .map_err(|error| {
                    VideoServiceError::io(
                        "video.publication_sync_failed",
                        "The staged import source could not be synchronized",
                        error,
                    )
                })?;
            let staging_c = CString::new(staging.as_os_str().as_bytes()).map_err(|_| {
                VideoServiceError::new(
                    "video.invalid_artifact_path",
                    "The import staging path contains an unsupported null byte",
                )
            })?;
            let final_c =
                CString::new(suggested_final_path.as_os_str().as_bytes()).map_err(|_| {
                    VideoServiceError::new(
                        "video.invalid_artifact_path",
                        "The managed import path contains an unsupported null byte",
                    )
                })?;
            let renamed = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    staging_c.as_ptr(),
                    libc::AT_FDCWD,
                    final_c.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if renamed != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    return inspect_bound(suggested_final_path, None);
                }
                if error.raw_os_error() == Some(libc::ENOSYS) {
                    match fs::hard_link(staging, suggested_final_path) {
                        Ok(()) => {
                            if let Err(remove_error) = fs::remove_file(staging) {
                                let _ = fs::remove_file(suggested_final_path);
                                return Err(VideoServiceError::io(
                                    "video.import_cleanup_failed",
                                    "The staged import link could not be finalized",
                                    remove_error,
                                ));
                            }
                        }
                        Err(link_error)
                            if link_error.kind() == std::io::ErrorKind::AlreadyExists =>
                        {
                            return inspect_bound(suggested_final_path, None);
                        }
                        Err(link_error) => {
                            return Err(VideoServiceError::io(
                                "video.publication_commit_failed",
                                "The import source could not be atomically published",
                                link_error,
                            ));
                        }
                    }
                } else {
                    return Err(VideoServiceError::io(
                        "video.publication_commit_failed",
                        "The import source could not be atomically published",
                        error,
                    ));
                }
            }
            if let Err(error) =
                secure_managed_file(suggested_final_path).and_then(|_| sync_directory(&source_dir))
            {
                let _ = fs::remove_file(suggested_final_path);
                return Err(error);
            }
            Ok((suggested_final_path.to_path_buf(), true))
        })();
        if result.is_err() {
            let _ = fs::remove_file(staging);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_local_source(
        &self,
        job_id: &str,
        project_id: &str,
        asset_id: &str,
        source_path: &Path,
        source_file: File,
        source_identity: LocalSourceIdentity,
        initial_probe: RuntimeMediaProbe,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<PreparedManagedFile> {
        if initial_probe.primary_video_stream.is_none()
            && initial_probe.primary_audio_stream.is_none()
        {
            return Err(VideoServiceError::new(
                "video.invalid_source",
                "The selected media contains no playable audio or video",
            ));
        }
        let extension = safe_extension(source_path, initial_probe.primary_video_stream.is_some());
        let source_dir = self.project_dir(project_id)?.join("sources");
        self.secure_managed_directory(&source_dir)?;
        let final_path = source_dir.join(format!("source-{asset_id}.{extension}"));
        let staging = sibling_staging_path(&final_path)?;
        let checksum =
            copy_file_cancelable(source_file, source_identity, &staging, cancel, |fraction| {
                let progress = 0.05 + fraction * 0.18;
                let _ = self.store.update_job(job_id, "running", progress);
                emit_progress(
                    callback,
                    job_id,
                    project_id,
                    "copying",
                    progress,
                    "Copying media into managed storage",
                    None,
                    None,
                );
            })?;
        let runtime = self.runtime_status(false);
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate local media",
        )?;
        let (published_path, _) = self.publish_import_staging_once(
            project_id,
            asset_id,
            &staging,
            &final_path,
            &checksum,
            cancel,
            |path| probe_media(path, ffprobe).map(|_| ()),
        )?;
        let final_probe = probe_media(&published_path, ffprobe)?;
        Ok(PreparedManagedFile {
            relative_path: self.relative_managed_path(&published_path)?,
            path: published_path,
            sha256: checksum,
            probe: final_probe,
        })
    }

    /// Store upsert is intentionally general-purpose and mutable. Durable
    /// import identities are stricter: once a child owns an asset id, no retry
    /// may rewrite its path, bytes, probe, or provenance. Status alone may
    /// advance from pending to ready after the guarded manifest commit.
    fn upsert_import_asset_compatible(&self, record: &Value) -> ServiceResult<Value> {
        let project_id = value_string(record, "project_id")?;
        let asset_id = value_string(record, "id")?;
        let existing = self
            .store
            .list_video_assets(&project_id)
            .map_err(VideoServiceError::store)?
            .into_iter()
            .find(|asset| asset.get("id").and_then(Value::as_str) == Some(asset_id.as_str()));
        if let Some(existing) = existing {
            const IMMUTABLE_FIELDS: [&str; 10] = [
                "kind",
                "source_kind",
                "local_path",
                "original_url",
                "mime_type",
                "content_sha256",
                "size_bytes",
                "duration_us",
                "probe",
                "provenance",
            ];
            let mismatches = IMMUTABLE_FIELDS
                .iter()
                .copied()
                .filter(|field| {
                    existing.get(*field).cloned().unwrap_or(Value::Null)
                        != record.get(*field).cloned().unwrap_or(Value::Null)
                })
                .collect::<Vec<_>>();
            if !mismatches.is_empty() {
                return Err(VideoServiceError::new(
                    "video.import_identity_conflict",
                    "A durable import asset id is already bound to different media",
                )
                .details(json!({
                    "asset_id": asset_id,
                    "mismatched_fields": mismatches,
                })));
            }
            let existing_status = existing.get("status").and_then(Value::as_str);
            let requested_status = record.get("status").and_then(Value::as_str);
            if existing_status == Some("ready") && requested_status == Some("pending") {
                return Ok(existing);
            }
            if existing_status == requested_status {
                return Ok(existing);
            }
        }
        self.store
            .upsert_video_asset(record)
            .map_err(VideoServiceError::store)
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_managed_source(
        &self,
        job_id: &str,
        project_id: &str,
        actor: &str,
        source: ManagedSource,
        runtime: &MediaRuntimeStatus,
        required_active_job_id: Option<&str>,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<Value> {
        self.ensure_not_cancelled(cancel)?;
        let ffmpeg = required_tool_path(
            &runtime.ffmpeg,
            "video.ffmpeg_unavailable",
            "FFmpeg is required to prepare source media",
        )?;
        let ffprobe = required_tool_path(
            &runtime.ffprobe,
            "video.ffprobe_unavailable",
            "FFprobe is required to validate source media",
        )?;
        let has_video = source.probe.primary_video_stream.is_some();
        let has_audio = source.probe.primary_audio_stream.is_some();
        let source_asset_record = json!({
            "id": source.id,
            "project_id": project_id,
            "kind": match source.kind {
                SourceAssetKind::SoundArSpeech => "speech",
                SourceAssetKind::SoundArMusic => "music",
                _ => "source",
            },
            "source_kind": match source.kind {
                SourceAssetKind::ImportedLink => "link",
                SourceAssetKind::SoundArSpeech
                | SourceAssetKind::SoundArMusic
                | SourceAssetKind::SoundArProject
                | SourceAssetKind::Generated => "generated",
                _ => "local",
            },
            "local_path": source.path,
            "original_url": source.provenance.original_uri,
            "mime_type": media_mime(&source.path, has_video),
            "content_sha256": source.sha256,
            "size_bytes": fs::metadata(&source.path).map(|metadata| metadata.len() as i64).ok(),
            "duration_us": source.probe.duration_us,
            // Pending rows are durable child-owned preparation. They become
            // ready only after the manifest transaction references them.
            "status": "pending",
            "probe": source.probe,
            "provenance": source.provenance,
        });
        self.upsert_import_asset_compatible(&source_asset_record)?;

        let mut derived = Vec::new();
        if has_video {
            let proxy = self.ensure_proxy(
                job_id, project_id, &source, ffmpeg, ffprobe, runtime, cancel, callback,
            )?;
            emit_progress(
                callback,
                job_id,
                project_id,
                "proxy_ready",
                0.54,
                "Fast preview proxy is ready",
                Some(proxy.playable_value()),
                None,
            );
            derived.push(proxy);
            let thumbnail = self.ensure_thumbnail(
                job_id, project_id, &source, ffmpeg, runtime, cancel, callback,
            )?;
            derived.push(thumbnail);
        }
        if has_audio {
            let waveform = self.ensure_waveform(
                job_id, project_id, &source, ffmpeg, runtime, cancel, callback,
            )?;
            derived.push(waveform);
        }
        for product in &mut derived {
            product.id = stable_import_asset_id(
                project_id,
                job_id,
                &format!("derived-{}", product.kind),
                &product.cache_key,
            );
        }
        self.ensure_not_cancelled(cancel)?;
        let mut derived_asset_records = Vec::with_capacity(derived.len());
        for product in &derived {
            let record = json!({
                "id": product.id,
                "project_id": project_id,
                "kind": product.kind,
                "source_kind": "derived",
                "local_path": product.path,
                "mime_type": product.mime_type,
                "content_sha256": product.sha256,
                "size_bytes": fs::metadata(&product.path).map(|metadata| metadata.len() as i64).ok(),
                "duration_us": product.duration_us,
                "status": "pending",
                "probe": product.probe,
                "provenance": {
                    "producer": "soundAr Video Studio",
                    "producer_version": SERVICE_VERSION,
                    "source_asset_id": source.id,
                    "cache_key": product.cache_key,
                },
            });
            self.upsert_import_asset_compatible(&record)?;
            derived_asset_records.push(record);
        }
        let publish_asset_rows = || -> ServiceResult<Value> {
            let mut ready_source = source_asset_record.clone();
            ready_source["status"] = Value::String("ready".to_string());
            let source_value = self.upsert_import_asset_compatible(&ready_source)?;
            for pending in &derived_asset_records {
                let mut ready = pending.clone();
                ready["status"] = Value::String("ready".to_string());
                self.upsert_import_asset_compatible(&ready)?;
            }
            Ok(source_value)
        };
        #[cfg(test)]
        if let Some(barrier) = self
            .local_import_test_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            // One-shot crash/cancellation boundary after every deterministic
            // asset row exists but before the guarded manifest transaction.
            barrier.wait();
            barrier.wait();
        }
        self.ensure_not_cancelled(cancel)?;

        let typed_source = typed_source_asset(&source)?;
        let source_rights = source.rights.clone();
        let artifacts = derived
            .iter()
            .map(|product| self.typed_render_artifact(product, &source.provenance.imported_at))
            .collect::<ServiceResult<Vec<_>>>()?;
        let current_project = self.get_project(project_id)?;
        let current_manifest: VideoProjectManifest = serde_json::from_value(
            current_project
                .get("manifest")
                .cloned()
                .ok_or_else(|| invalid_store_shape("manifest"))?,
        )
        .map_err(json_error)?;
        current_manifest.validate_strict()?;
        if let Some(existing_source) = current_manifest
            .source_assets
            .iter()
            .find(|existing| existing.id == typed_source.id)
        {
            let artifacts_match = artifacts.iter().all(|artifact| {
                current_manifest
                    .render_artifacts
                    .iter()
                    .any(|existing| existing == artifact)
            });
            let rights_match = source_rights.as_ref().is_none_or(|rights| {
                current_manifest
                    .rights_confirmations
                    .iter()
                    .any(|existing| existing == rights)
            });
            if existing_source != &typed_source || !artifacts_match || !rights_match {
                return Err(VideoServiceError::new(
                    "video.import_identity_conflict",
                    "The durable import identity is already bound to different project media",
                )
                .details(json!({
                    "source_asset_id": typed_source.id,
                    "job_id": job_id,
                })));
            }
            let current_expectation = project_expectation(&current_project)?;
            let source_value = publish_asset_rows()?;
            self.checkpoint_stage(
                project_id,
                Some(&current_expectation.version_id),
                "ingest",
                "source",
                job_id,
                "completed",
                "medium",
                1.0,
                &source.sha256,
                Some(&source.sha256),
                json!({
                    "source_asset_id": source.id,
                    "derived_count": derived.len(),
                    "idempotent_replay": true,
                }),
                None,
            )?;
            emit_progress(
                callback,
                job_id,
                project_id,
                "source_ready",
                0.98,
                "The durable source import was recovered without duplication",
                Some(source_value),
                Some(json!({ "idempotent_replay": true })),
            );
            return Ok(current_project);
        }
        let source_id = source.id.clone();
        let duration_us = source.probe.duration_us;
        let track_kind = if has_video {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let expectation = project_expectation(&current_project)?;
        let committed = self.commit_manifest_mutation_at_if_parent_active(
            project_id,
            &expectation,
            actor,
            "Imported source media and prepared reusable preview assets",
            Some("ready"),
            vec![
                "/source_assets".to_string(),
                "/rights_confirmations".to_string(),
                "/tracks".to_string(),
                "/render_artifacts".to_string(),
            ],
            BTreeSet::from([
                RevisionStage::Ingest,
                RevisionStage::Transcript,
                RevisionStage::Analysis,
                RevisionStage::Plan,
                RevisionStage::Captions,
                RevisionStage::SceneRender,
                RevisionStage::Preview,
                RevisionStage::FinalRender,
                RevisionStage::PublishPackage,
            ]),
            required_active_job_id,
            move |manifest| {
                let first_source = manifest.source_assets.is_empty();
                if let Some(rights) = source_rights {
                    manifest.rights_confirmations.push(rights);
                }
                manifest.source_assets.push(typed_source);
                manifest.render_artifacts.extend(artifacts);
                if first_source && manifest.tracks.is_empty() {
                    manifest.timeline_duration_us = Microseconds(duration_us);
                    manifest.tracks.push(TimelineTrack {
                        id: format!("source-track-{source_id}"),
                        kind: track_kind,
                        clips: vec![TimelineClip {
                            id: format!("source-clip-{source_id}"),
                            scene_id: None,
                            turn_id: None,
                            media: super::MediaReference {
                                source_asset_id: Some(source_id.clone()),
                                render_artifact_id: None,
                            },
                            source_range: TimeRange::new(0, duration_us)?,
                            timeline_start_us: Microseconds::ZERO,
                            timeline_duration_us: Microseconds(duration_us),
                            playback_rate: RationalRate::ONE,
                            gain_db_milli: 0,
                            muted: false,
                            crop: None,
                        }],
                        preserve_gaps: true,
                    });
                }
                Ok(())
            },
        )?;
        let source_value = publish_asset_rows()?;
        let version_id = committed
            .get("version")
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.checkpoint_stage(
            project_id,
            version_id.as_deref(),
            "ingest",
            "source",
            job_id,
            "completed",
            "medium",
            1.0,
            &source.sha256,
            Some(&source.sha256),
            json!({ "source_asset_id": source.id, "derived_count": derived.len() }),
            None,
        )?;
        let _ = self.store.update_job(job_id, "running", 0.98);
        emit_progress(
            callback,
            job_id,
            project_id,
            "source_ready",
            0.98,
            "Source, timeline and previews are ready",
            Some(source_value),
            None,
        );
        Ok(committed)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_proxy(
        &self,
        job_id: &str,
        project_id: &str,
        source: &ManagedSource,
        ffmpeg: &Path,
        ffprobe: &Path,
        runtime: &MediaRuntimeStatus,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<DerivedProduct> {
        let cache_key = media_cache_key(
            CacheStage::Proxy,
            &source.sha256,
            runtime,
            json!({
                "profile": "proxy",
                "width": 960,
                "height": 540,
            }),
        )?;
        if let Some(product) = self.cached_product(
            project_id,
            &cache_key,
            "proxy",
            "video/mp4",
            RenderArtifactRole::Proxy,
            Some(ffprobe),
        )? {
            self.record_cache_hit(project_id, job_id, "proxy_generation");
            return Ok(product);
        }
        let output = self.cache_path("proxy", &cache_key, "mp4")?;
        if output.is_file() {
            let probe = probe_media(&output, ffprobe)?;
            let _ = self.store.put_video_cache(
                &cache_key,
                "proxy",
                Some(project_id),
                &json!({ "source_sha256": source.sha256, "profile": "proxy" }),
                &output,
            );
            return product_from_media(
                &self.video_root,
                output,
                cache_key,
                "proxy",
                "video/mp4",
                RenderArtifactRole::Proxy,
                probe,
            );
        }
        let staging = sibling_staging_path(&output)?;
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:proxy:{cache_key}"),
            output.parent().unwrap_or(&self.video_root),
            with_disk_headroom(
                estimated_render_bytes(source.probe.duration_us, RenderProfile::Proxy)?,
                1,
            ),
            "proxy_generation",
        )?;
        let plan = build_proxy_command(
            ffmpeg,
            &source.path,
            &staging,
            RenderProfile::Proxy,
            runtime.h264_nvenc_runtime,
        )?;
        self.execute_render_plan(
            job_id,
            project_id,
            &plan,
            Some(source.probe.duration_us),
            0.25,
            0.52,
            cancel,
            callback,
        )?;
        publish_atomic(&staging, &output, |path| {
            probe_media(path, ffprobe).and_then(|probe| {
                if probe.primary_video_stream.is_none() {
                    Err(MediaError::new(
                        "invalid_proxy",
                        "The generated proxy has no video stream",
                    ))
                } else {
                    Ok(())
                }
            })
        })?;
        self.store
            .put_video_cache(
                &cache_key,
                "proxy",
                Some(project_id),
                &json!({ "source_sha256": source.sha256, "profile": "proxy" }),
                &output,
            )
            .map_err(VideoServiceError::store)?;
        let probe = probe_media(&output, ffprobe)?;
        product_from_media(
            &self.video_root,
            output,
            cache_key,
            "proxy",
            "video/mp4",
            RenderArtifactRole::Proxy,
            probe,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_thumbnail(
        &self,
        job_id: &str,
        project_id: &str,
        source: &ManagedSource,
        ffmpeg: &Path,
        runtime: &MediaRuntimeStatus,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<DerivedProduct> {
        let at_us = (source.probe.duration_us / 5).clamp(0, 3_000_000);
        let cache_key = media_cache_key(
            CacheStage::Thumbnail,
            &source.sha256,
            runtime,
            json!({
                "at_us": at_us,
                "width": 640,
                "height": 360,
            }),
        )?;
        if let Some(product) = self.cached_product(
            project_id,
            &cache_key,
            "thumbnail",
            "image/jpeg",
            RenderArtifactRole::Thumbnail,
            None,
        )? {
            self.record_cache_hit(project_id, job_id, "thumbnail_generation");
            return Ok(product);
        }
        let output = self.cache_path("thumbnail", &cache_key, "jpg")?;
        if !output.is_file() {
            let staging = sibling_staging_path(&output)?;
            let mut plan = build_thumbnail_command(ffmpeg, &source.path, &staging, at_us)?;
            force_image_codec(&mut plan, "mjpeg");
            self.execute_render_plan(
                job_id, project_id, &plan, None, 0.54, 0.64, cancel, callback,
            )?;
            publish_atomic(&staging, &output, validate_image_file)?;
        } else {
            validate_image_file(&output)?;
        }
        self.store
            .put_video_cache(
                &cache_key,
                "thumbnail",
                Some(project_id),
                &json!({ "source_sha256": source.sha256, "at_us": at_us }),
                &output,
            )
            .map_err(VideoServiceError::store)?;
        product_from_image(
            &self.video_root,
            output,
            cache_key,
            "thumbnail",
            "image/jpeg",
            RenderArtifactRole::Thumbnail,
            Some((640, 360)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_waveform(
        &self,
        job_id: &str,
        project_id: &str,
        source: &ManagedSource,
        ffmpeg: &Path,
        runtime: &MediaRuntimeStatus,
        cancel: &AtomicBool,
        callback: Option<&ProgressCallback>,
    ) -> ServiceResult<DerivedProduct> {
        let cache_key = media_cache_key(
            CacheStage::Waveform,
            &source.sha256,
            runtime,
            json!({
                "width": 1600,
                "height": 320,
                "color": "neutral-600",
            }),
        )?;
        if let Some(product) = self.cached_product(
            project_id,
            &cache_key,
            "waveform",
            "image/png",
            RenderArtifactRole::Waveform,
            None,
        )? {
            self.record_cache_hit(project_id, job_id, "waveform_generation");
            return Ok(product);
        }
        let output = self.cache_path("waveform", &cache_key, "png")?;
        if !output.is_file() {
            let staging = sibling_staging_path(&output)?;
            let mut plan = build_waveform_command(ffmpeg, &source.path, &staging, 1600, 320)?;
            force_image_codec(&mut plan, "png");
            self.execute_render_plan(
                job_id,
                project_id,
                &plan,
                Some(source.probe.duration_us),
                0.65,
                0.76,
                cancel,
                callback,
            )?;
            publish_atomic(&staging, &output, validate_image_file)?;
        } else {
            validate_image_file(&output)?;
        }
        self.store
            .put_video_cache(
                &cache_key,
                "waveform",
                Some(project_id),
                &json!({ "source_sha256": source.sha256, "width": 1600, "height": 320 }),
                &output,
            )
            .map_err(VideoServiceError::store)?;
        product_from_image(
            &self.video_root,
            output,
            cache_key,
            "waveform",
            "image/png",
            RenderArtifactRole::Waveform,
            Some((1600, 320)),
        )
    }
}
