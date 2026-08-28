//! Production orchestration for soundAr Video Studio.
//!
//! This is the single service used by native commands and Codex tools.  It owns
//! workflow admission/cancellation, but deliberately delegates persistence to
//! [`Store`] and deterministic media planning to the sibling video modules.

use super::{
    build_ass_document, build_portrait_command_with_layout, build_proxy_command,
    build_thumbnail_command, build_timeline_render_plan, build_waveform_command,
    discover_media_runtime, local_media_input_args, preflight_import_url_destination, probe_media,
    publish_atomic, sibling_staging_path, terminate_process_group, validate_import_url,
    write_ass_document_atomic, AdmissionOutcome, AssemblyOptions, CacheKeyBuilder, CacheStage,
    CaptionTheme, FfmpegProgressParser, MediaError, MediaRuntimeStatus, Microseconds,
    NarrationBinding, PortraitLayout, Provenance, ProvenanceKind, PublicHttpsProxy,
    PublicationState, RationalFrameRate, RationalRate, RenderArtifact, RenderArtifactRole,
    RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass, ResourceClass,
    ResourceRequest, ResourceScheduler, RevisionRecord, RevisionStage, RightsBasis,
    RightsConfirmation, RuntimeMediaProbe, SourceAsset, SourceAssetKind, TimeRange, TimelineClip,
    TimelineTrack, TrackKind, Validate, VideoEncoder, VideoError, VideoProjectManifest,
};
use crate::store::Store;
use chrono::{SecondsFormat, Utc};
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

const SERVICE_VERSION: &str = "video-service-v1";
const PROJECT_LOCK_SECONDS: i64 = 120;
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 128 * 1024;
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);
const COMMAND_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const RESOURCE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LINK_PREVIEW_TIMEOUT: Duration = Duration::from_secs(45);
const LINK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PACKAGE_AGGREGATE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_PACKAGE_ARCHIVE_BYTES: u64 = MAX_PACKAGE_AGGREGATE_BYTES + 64 * 1024 * 1024;
const PACKAGE_METADATA_RESERVE_BYTES: u64 = 64 * 1024 * 1024;
const DISK_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const SHAREABLE_FILE_MODE: u32 = 0o644;
const MAX_RENDER_DURATION: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_MEDIA_DURATION_US: i64 = 6 * 60 * 60 * 1_000_000;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
    pub history_id: String,
    pub voice_id: String,
    pub model_id: String,
    pub speaker: String,
    pub language: String,
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
    #[serde(default = "default_normal_priority")]
    priority: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectExpectation {
    revision: i64,
    version_id: String,
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
        let status = self
            .store
            .start_job(&job_id)
            .map_err(VideoServiceError::store)?;
        if status == "cancelled" {
            return Err(VideoServiceError::cancelled());
        }
        let cancel = AtomicBool::new(false);
        let result = self.perform_publish_package(&job_id, &request, project, &cancel);
        match result {
            Ok(result) => {
                self.store
                    .complete_job(&job_id)
                    .map_err(VideoServiceError::store)?;
                Ok(json!({
                    "job_id": job_id,
                    "project_id": request.project_id,
                    "output": result.output,
                    "package_path": result.package_path,
                    "archive_path": result.archive_path,
                    "export_path": result.export_path,
                }))
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
        let version_sha = project
            .get("version")
            .and_then(|value| value.get("sha256"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
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
            .profile(json!({ "format": "publish-zip-v2" }))
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
        validate_package_identity(&package_path, &cache_key, &master_sha, Some(cancel))?;
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
                &json!({ "master_sha256": master_sha, "version_sha256": version_sha }),
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
        let output_id = stable_output_id(&request.project_id, "publish-package", &cache_key, 0);
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
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    mut progress: F,
) -> ServiceResult<String>
where
    F: FnMut(f64),
{
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected media could not be opened safely",
                error,
            )
        })?;
    let total = input
        .metadata()
        .map_err(|error| {
            VideoServiceError::io(
                "video.source_not_found",
                "The selected media could not be inspected",
                error,
            )
        })?
        .len();
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

/// Stable semantic identity for a published output. The content-addressed
/// cache key makes equivalent durable retries converge across artifact-only
/// manifest versions, while the role and variation discriminator keep
/// intentionally distinct outputs separate even when FFmpeg happens to emit
/// byte-identical media.
fn stable_output_id(project_id: &str, role: &str, cache_key: &str, variation: u16) -> String {
    let identity = json!([project_id, role, cache_key, variation]).to_string();
    format!("video-output-{}", sha256_bytes(identity.as_bytes()))
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

fn validate_image_file(path: &Path) -> Result<(), MediaError> {
    let metadata = fs::metadata(path).map_err(|error| {
        MediaError::new("image_not_found", "The generated image could not be opened")
            .detail(error.to_string())
    })?;
    if !metadata.is_file() || metadata.len() < 16 {
        return Err(MediaError::new(
            "invalid_image",
            "The generated image is empty or invalid",
        ));
    }
    let mut header = [0_u8; 12];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| {
            MediaError::new(
                "invalid_image",
                "The generated image could not be validated",
            )
            .detail(error.to_string())
        })?;
    let png = header.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let jpeg = header.starts_with(&[0xff, 0xd8, 0xff]);
    if !png && !jpeg {
        return Err(MediaError::new(
            "invalid_image",
            "The generated image is not PNG or JPEG data",
        ));
    }
    Ok(())
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
            });
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
        VideoEncoder::Image => Vec::new(),
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
    if cache_key != Some(expected_cache_key) || master_sha256 != Some(expected_master_sha256) {
        return Err(VideoServiceError::new(
            "video.package_identity_mismatch",
            "An existing package does not belong to this render and was not reused",
        ));
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
    let current = project_expectation(project)?;
    match (expected_revision, expected_version_id) {
        (None, None) => Ok(current),
        (Some(revision), Some(version_id)) => {
            let requested = ProjectExpectation {
                revision,
                version_id: version_id.to_string(),
            };
            ensure_project_matches(project, &requested)?;
            Ok(requested)
        }
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
    CacheKeyBuilder::new(CacheStage::Captions, format!("{SERVICE_VERSION}:ass-v2"))
        .manifest_slice(json!({
            "reviewed_scenes": &manifest.reviewed_scenes,
            "captions": &manifest.captions,
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

fn actual_manifest_changed_paths(
    before: &VideoProjectManifest,
    after: &VideoProjectManifest,
) -> ServiceResult<BTreeSet<String>> {
    let before = manifest_content_value(before)?;
    let after = manifest_content_value(after)?;
    let mut changed = BTreeSet::new();
    collect_manifest_diff(&before, &after, "", &mut changed);
    Ok(changed)
}

fn inferred_invalidated_stages(paths: &BTreeSet<String>) -> BTreeSet<RevisionStage> {
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
        } else if path.starts_with("/captions") {
            BTreeSet::from([
                RevisionStage::Captions,
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
                scene_id: Some(scene_id.clone()),
                clip_id: None,
                history_id: history_id.clone(),
                voice_id: "voice-new".to_string(),
                model_id: "test/voice-model".to_string(),
                speaker: "speaker-new".to_string(),
                language: "en".to_string(),
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
                scene_id: Some(scene_id.clone()),
                clip_id: None,
                history_id: history_id.clone(),
                voice_id: "voice-cancelled-before-commit".to_string(),
                model_id: "test/voice-model".to_string(),
                speaker: "speaker-new".to_string(),
                language: "en".to_string(),
            }],
        };
        let cancellation_observed = Arc::new(AtomicBool::new(false));
        let callback_service = Arc::clone(&workspace.service);
        let callback_parent_id = cancel_parent_job_id.clone();
        let callback_observed = Arc::clone(&cancellation_observed);
        let cancel_callback: ProgressCallback = Arc::new(move |progress| {
            if progress.phase == "narration_take_ready"
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
            "the test must cancel after the prepared take and before manifest commit"
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

        // Cancel exactly at the first durable progress event. This leaves the
        // queue-produced request available for a restart-style resume without
        // importing or duplicating the source first.
        let cancel_service = Arc::clone(&workspace.service);
        let cancel_once = Arc::new(AtomicBool::new(false));
        let callback_once = Arc::clone(&cancel_once);
        let cancel_callback: ProgressCallback = Arc::new(move |progress| {
            if progress.phase == "validating" && !callback_once.swap(true, Ordering::AcqRel) {
                let _ = cancel_service.cancel_job(&progress.job_id);
            }
        });
        let queued = workspace
            .service
            .queue_local_import(
                LocalImportRequest {
                    project_id: project_id.clone(),
                    source_path: generated_audio.clone(),
                    actor: "service-test".to_string(),
                    title: Some("Prompt narration".to_string()),
                },
                Some(cancel_callback),
            )
            .expect("queue prompt-generated History import");
        let cancelled = workspace
            .service
            .wait_for_job(&queued.job_id, &project_id, Duration::from_secs(30))
            .expect_err("first import attempt is cancelled before copy");
        assert_eq!(cancelled.code, "video.cancelled");
        assert!(cancel_once.load(Ordering::Acquire));

        let (resumed_job, durable_value) = workspace
            .service
            .store
            .resume_video_job(&queued.job_id, &["video_import_local"])
            .expect("rearm durable generated import");
        assert_eq!(
            resumed_job.get("status").and_then(Value::as_str),
            Some("preparing"),
            "the durable queue presentation should return to its active initial phase"
        );
        let durable: DurableLocalImportRequest =
            serde_json::from_value(durable_value.clone()).expect("typed durable local import");
        assert_eq!(durable.source_path, generated_audio);
        assert_eq!(
            durable.origin,
            DurableLocalImportOrigin::SoundArHistory {
                history_id: history_id.clone(),
                generation_job_id: synthesis_job.clone(),
                generation_kind: "speech".to_string(),
                model_id: "test/prompt-voice-model".to_string(),
                voice: "Prompt voice".to_string(),
                engine: "service-test-prompt-engine".to_string(),
            },
            "the queue must persist immutable History identity in the durable request"
        );
        workspace
            .service
            .dispatch_resumed_job(
                queued.job_id.clone(),
                "video_import_local",
                durable_value,
                None,
            )
            .expect("dispatch restart-style local import");
        let imported = workspace
            .service
            .wait_for_job(&queued.job_id, &project_id, Duration::from_secs(120))
            .expect("resumed generated import completes");
        let manifest: VideoProjectManifest =
            serde_json::from_value(imported.project.get("manifest").cloned().expect("manifest"))
                .expect("typed generated import manifest");
        let source = manifest
            .source_assets
            .iter()
            .find(|source| matches!(source.kind, SourceAssetKind::SoundArSpeech))
            .expect("soundAr speech source");
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
        let assets = workspace
            .service
            .store
            .list_video_assets(&project_id)
            .expect("list generated video assets");
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

        let tamper_project_id = format!("project-{}", new_id());
        workspace.create_project(&tamper_project_id, "Tampered History rejection");
        let tamper_project_before = workspace
            .service
            .get_project(&tamper_project_id)
            .expect("tamper project baseline");
        let mut tampered_request = durable;
        tampered_request.project_id = tamper_project_id.clone();
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
            expected_version_id: reviewed_expectation.version_id,
            profile: TimelineRenderProfile::Preview,
            caption_theme: CaptionTheme::Calm,
            portrait_layout: PortraitSourceLayout::CenterCrop,
            actor: "service-test".to_string(),
            variation: 0,
            include_title_cards: true,
            include_speaker_cards: true,
            burn_captions: true,
        };
        let timeline_job = workspace
            .service
            .queue_timeline_render(timeline_request, None)
            .expect("queue reviewed timeline");
        let first_timeline = workspace
            .service
            .wait_for_job(
                &timeline_job.job_id,
                &video_project,
                Duration::from_secs(180),
            )
            .expect("reviewed timeline completes");
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

        let crash_base = project_expectation(&replay.project).expect("crash batch base");
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
        workspace
            .service
            .wait_for_job(&import.job_id, &audio_project, Duration::from_secs(120))
            .expect("audio import completes");
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
        let rendered = workspace
            .service
            .wait_for_job(&render.job_id, &audio_project, Duration::from_secs(180))
            .expect("audio portrait completes");
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
                project_id: audio_project,
                expected_revision: None,
                expected_version_id: None,
                destination_dir: None,
                actor: "service-test".to_string(),
            })
            .expect_err("an older-version master must not be packaged for a revised timeline");
        assert_eq!(stale_error.code, "video.final_master_required");
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
        let result = self
            .store
            .commit_video_manifest(
                project_id,
                expected_revision,
                &manifest_value,
                actor,
                reason,
                &lock.token,
                status,
            )
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

    fn typed_render_artifact(&self, product: &DerivedProduct) -> ServiceResult<RenderArtifact> {
        let artifact = RenderArtifact {
            id: new_id(),
            role: product.role.clone(),
            scene_id: None,
            managed_path: self.relative_managed_path(&product.path)?,
            sha256: product.sha256.clone(),
            cache_key: product.cache_key.clone(),
            mime_type: product.mime_type.clone(),
            duration_us: product.duration_us.map(Microseconds),
            width: product.width,
            height: product.height,
            publication_state: PublicationState::Published,
            created_at: utc_now(),
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
        let actual_paths = actual_manifest_changed_paths(&current_manifest, &request.manifest)?;
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
        let inferred_stages = inferred_invalidated_stages(&actual_paths);
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
            thumbnail_url: metadata
                .get("thumbnail")
                .and_then(Value::as_str)
                .filter(|url| url.starts_with("https://"))
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
            priority: default_normal_priority(),
        };
        let job_id = self
            .store
            .create_job(
                "video_import_local",
                &serde_json::to_value(&durable_request).map_err(json_error)?,
            )
            .map_err(VideoServiceError::store)?;
        let project_id = durable_request.project_id.clone();
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
                .ok_or_else(|| {
                    VideoServiceError::new(
                        "video.narration_target_required",
                        "Select an exact narration binding, clip, or scene",
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
        let expectation = expectation_from_optional(
            &project,
            request.expected_revision,
            request.expected_version_id.as_deref(),
        )?;
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
        let stage_key = match request.profile {
            RenderProfile::Final => "final_render",
            RenderProfile::Proxy | RenderProfile::Preview => "preview_render",
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
        let artifact_for_manifest = artifact.clone();
        let committed = if manifest
            .render_artifacts
            .iter()
            .any(|existing| existing.cache_key == cache_key)
        {
            let current = self.get_project(&request.project_id)?;
            ensure_project_matches(&current, &expectation)?;
            current
        } else {
            self.commit_manifest_mutation_at(
                &request.project_id,
                &expectation,
                &request.actor,
                if request.profile == RenderProfile::Final {
                    "Published a final portrait master"
                } else {
                    "Published a fast portrait preview"
                },
                Some(if request.profile == RenderProfile::Final {
                    "completed"
                } else {
                    "ready"
                }),
                vec!["/render_artifacts".to_string()],
                BTreeSet::from([if request.profile == RenderProfile::Final {
                    RevisionStage::FinalRender
                } else {
                    RevisionStage::Preview
                }]),
                move |manifest| {
                    manifest.render_artifacts.push(artifact_for_manifest);
                    Ok(())
                },
            )?
        };
        let committed_expectation = project_expectation(&committed)?;
        let publication_lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(&current, &committed_expectation)?;
        let version_id = Some(committed_expectation.version_id.as_str());
        let output_kind = if request.variation > 0 {
            "variation"
        } else if request.profile == RenderProfile::Final {
            "master"
        } else {
            "preview"
        };
        let output_id = stable_output_id(
            &request.project_id,
            &format!("portrait-{output_kind}"),
            &cache_key,
            request.variation,
        );
        let output = self
            .store
            .publish_video_output_current_cancellable(
                &json!({
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
                        "source_asset_id": source.id,
                        "cache_key": cache_key,
                        "profile": request.profile,
                        "layout": request.layout,
                        "variation": request.variation,
                        "audio_only": !has_video,
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
            let target_clip = match candidate_clips.as_slice() {
                [clip] => clip.clone(),
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
            if !prepared_clip_ids.insert(target_clip.id.clone()) {
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
                .or_else(|| target_clip.scene_id.clone());
            if replacement.scene_id.is_some()
                && target_clip.scene_id.as_deref() != replacement.scene_id.as_deref()
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
            let script = scene
                .map(|scene| scene.script.as_str())
                .unwrap_or_else(|| history.get("text").and_then(Value::as_str).unwrap_or(""));
            let script_sha = sha256_bytes(script.as_bytes());
            if scene.is_some()
                && history
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| sha256_bytes(text.as_bytes()))
                    .as_deref()
                    != Some(script_sha.as_str())
            {
                return Err(VideoServiceError::new(
                    "video.narration_script_mismatch",
                    "The generated speech does not match the current reviewed scene script",
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
            let target_duration_us = target_clip.timeline_duration_us.0;
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
                "clip_id": target_clip.id,
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
                render_artifact_id: artifact.id.clone(),
                history_id: replacement.history_id.clone(),
                generation_job_id,
                voice_id: replacement.voice_id.clone(),
                model_id: replacement.model_id.clone(),
                speaker: replacement.speaker.clone(),
                language: replacement.language.clone(),
                script_sha256: script_sha,
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
                clip_id: target_clip.id,
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
        let committed = self.commit_manifest_mutation_at(
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
                        return Err(VideoServiceError::new(
                            "video.narration_target_not_found",
                            "The narration clip changed before the revision was committed",
                        ));
                    }
                    if !manifest
                        .render_artifacts
                        .iter()
                        .any(|artifact| artifact.id == replacement.artifact.id)
                    {
                        manifest.render_artifacts.push(replacement.artifact.clone());
                    }
                    manifest.narration_bindings.retain(|binding| {
                        replacement.replaced_binding_id.as_deref() != Some(binding.id.as_str())
                            && replacement.binding.scene_id.as_deref()
                                != binding.scene_id.as_deref()
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
                    saved
                        .iter()
                        .find(|output| {
                            output.get("version_id").and_then(Value::as_str)
                                == Some(committed_expectation.version_id.as_str())
                                && output.get("kind").and_then(Value::as_str) == Some(kind)
                                && output.get("sha256").and_then(Value::as_str)
                                    == Some(render.output_sha.as_str())
                        })
                        .cloned()
                        .ok_or_else(|| {
                            VideoServiceError::new(
                                "video.store_contract_failed",
                                "An atomically published batch output could not be reloaded",
                            )
                        })
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
        let project = self.get_project(&request.project_id)?;
        ensure_project_matches(&project, &expectation)?;
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

        let render_slice = json!({
            "reviewed_scenes": manifest.reviewed_scenes,
            "tracks": manifest.tracks,
            "gaps": manifest.gaps,
            "captions": manifest.captions,
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
        let stage_key = match request.profile {
            TimelineRenderProfile::Preview => "preview_render",
            TimelineRenderProfile::Final => "final_render",
        };
        let scope_key = format!("timeline-variation-{}", request.variation);
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
        let committed = if has_caption_artifact && has_render_artifact {
            let current = self.get_project(&request.project_id)?;
            ensure_project_matches(&current, &expectation)?;
            current
        } else {
            self.commit_manifest_mutation_at(
                &request.project_id,
                &expectation,
                &request.actor,
                match request.profile {
                    TimelineRenderProfile::Preview if request.variation > 0 => {
                        "Published a reviewed-timeline preview variation"
                    }
                    TimelineRenderProfile::Preview => "Published a reviewed-timeline preview",
                    TimelineRenderProfile::Final if request.variation > 0 => {
                        "Published an alternate reviewed final render"
                    }
                    TimelineRenderProfile::Final => "Published the reviewed final master",
                },
                Some(match request.profile {
                    TimelineRenderProfile::Preview => "ready",
                    TimelineRenderProfile::Final => "completed",
                }),
                vec!["/render_artifacts".to_string()],
                BTreeSet::from([match request.profile {
                    TimelineRenderProfile::Preview => RevisionStage::Preview,
                    TimelineRenderProfile::Final => RevisionStage::FinalRender,
                }]),
                move |manifest| {
                    if !has_caption_artifact {
                        manifest.render_artifacts.push(caption_artifact);
                    }
                    if !has_render_artifact {
                        manifest.render_artifacts.push(render_artifact);
                    }
                    Ok(())
                },
            )?
        };
        let committed_expectation = project_expectation(&committed)?;
        let publication_lock = ProjectLock::acquire(self, &request.project_id, &request.actor)?;
        let current = self.get_project(&request.project_id)?;
        ensure_project_matches(&current, &committed_expectation)?;
        let output_kind = match (request.profile, request.variation) {
            (_, variation) if variation > 0 => "variation",
            (TimelineRenderProfile::Preview, _) => "preview",
            (TimelineRenderProfile::Final, _) => "master",
        };
        let semantic_role = match request.profile {
            TimelineRenderProfile::Preview => "timeline-preview",
            TimelineRenderProfile::Final => "timeline-final",
        };
        let output_id = stable_output_id(
            &request.project_id,
            semantic_role,
            &render_key,
            request.variation,
        );
        let output = self
            .store
            .publish_video_output_current_cancellable(
                &json!({
                    "id": output_id,
                    "project_id": request.project_id,
                    "version_id": committed_expectation.version_id,
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
                        "manifest_revision": request.expected_revision,
                        "source_version_id": request.expected_version_id,
                        "render_cache_key": render_key,
                        "caption_cache_key": caption_key,
                        "profile": request.profile,
                        "variation": request.variation,
                        "caption_theme": request.caption_theme,
                        "portrait_layout": request.portrait_layout,
                        "nvenc_with_software_fallback": runtime.h264_nvenc_runtime,
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
        // Admission may wait behind another media workload. Recheck at the
        // actual copy boundary so an earlier UX preflight cannot go stale.
        let admitted_source_size = fs::symlink_metadata(&request.source_path)
            .map_err(|error| {
                VideoServiceError::io(
                    "video.source_not_found",
                    "The selected local media could not be re-inspected after admission",
                    error,
                )
            })?
            .len();
        validate_source_size(admitted_source_size)?;
        let _storage_lease = self.reserve_storage(
            format!("{job_id}:local-import"),
            &self.video_root,
            with_disk_headroom(admitted_source_size, 3),
            "local_import",
        )?;
        let asset_id = new_id();
        let managed = self.prepare_local_source(
            job_id,
            &request.project_id,
            &asset_id,
            &request.source_path,
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
                imported_at: utc_now(),
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
        let asset_id = new_id();
        let source_dir = self.project_dir(&request.project_id)?.join("sources");
        self.secure_managed_directory(&source_dir)?;
        let prefix = format!(".download-{}", new_id());
        let output_template = source_dir.join(format!("{prefix}.%(ext)s"));
        preflight_import_url_destination(&validated)?;
        let proxy = PublicHttpsProxy::start()?;
        let args = vec![
            OsString::from("--ignore-config"),
            OsString::from("--no-playlist"),
            OsString::from("--max-downloads"),
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
            OsString::from(proxy.url()),
            OsString::from("--output"),
            output_template.as_os_str().to_os_string(),
            OsString::from("--"),
            OsString::from(canonical_url),
        ];
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
            let publication = publish_atomic(&downloaded, &final_path, |path| {
                probe_media(path, ffprobe).map(|_| ())
            })?;
            published_path = Some(publication.path.clone());
            secure_managed_file(&publication.path)?;
            self.ensure_not_cancelled(cancel)?;
            let checksum = sha256_file_with_cancel(&publication.path, Some(cancel))?;
            let final_probe = probe_media(&publication.path, ffprobe)?;
            validate_source_size(final_probe.size_bytes)?;
            validate_media_duration(final_probe.duration_us)?;
            let relative_path = self.relative_managed_path(&publication.path)?;
            Ok((publication.path, relative_path, checksum, final_probe))
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
                imported_at: utc_now(),
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

    #[allow(clippy::too_many_arguments)]
    fn prepare_local_source(
        &self,
        job_id: &str,
        project_id: &str,
        asset_id: &str,
        source_path: &Path,
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
        let checksum = copy_file_cancelable(source_path, &staging, cancel, |fraction| {
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
        let publication = publish_atomic(&staging, &final_path, |path| {
            probe_media(path, ffprobe).map(|_| ())
        })?;
        let final_probe = probe_media(&publication.path, ffprobe)?;
        Ok(PreparedManagedFile {
            relative_path: self.relative_managed_path(&publication.path)?,
            path: publication.path,
            sha256: checksum,
            probe: final_probe,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_managed_source(
        &self,
        job_id: &str,
        project_id: &str,
        actor: &str,
        source: ManagedSource,
        runtime: &MediaRuntimeStatus,
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
        let source_value = self
            .store
            .upsert_video_asset(&json!({
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
                "status": "ready",
                "probe": source.probe,
                "provenance": source.provenance,
            }))
            .map_err(VideoServiceError::store)?;

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
        self.ensure_not_cancelled(cancel)?;
        for product in &derived {
            self.store
                .upsert_video_asset(&json!({
                    "id": product.id,
                    "project_id": project_id,
                    "kind": product.kind,
                    "source_kind": "derived",
                    "local_path": product.path,
                    "mime_type": product.mime_type,
                    "content_sha256": product.sha256,
                    "size_bytes": fs::metadata(&product.path).map(|metadata| metadata.len() as i64).ok(),
                    "duration_us": product.duration_us,
                    "status": "ready",
                    "probe": product.probe,
                    "provenance": {
                        "producer": "soundAr Video Studio",
                        "producer_version": SERVICE_VERSION,
                        "source_asset_id": source.id,
                        "cache_key": product.cache_key,
                    },
                }))
                .map_err(VideoServiceError::store)?;
        }

        let typed_source = typed_source_asset(&source)?;
        let source_rights = source.rights.clone();
        let artifacts = derived
            .iter()
            .map(|product| self.typed_render_artifact(product))
            .collect::<ServiceResult<Vec<_>>>()?;
        let source_id = source.id.clone();
        let duration_us = source.probe.duration_us;
        let track_kind = if has_video {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        let committed = self.commit_manifest_mutation(
            project_id,
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
