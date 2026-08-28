//! Shared, renderer-independent contracts for soundAr Video Studio.
//!
//! The eventual `VideoStudioService` should accept and return these types from every
//! surface (Tauri commands, Codex tools, and any future CLI/HTTP adapter). Media execution,
//! persistence, and transport deliberately sit outside this module.

pub mod assembly;
pub mod cache;
pub mod contracts;
pub mod intelligence;
pub mod media;
pub mod presentation;
pub mod renderer;
pub mod scheduler;
pub mod service;
pub mod timeline;

pub use assembly::{
    build_ass_document, build_timeline_render_plan, write_ass_document_atomic, AssemblyOptions,
    CaptionTheme,
};
pub use cache::{
    CacheArtifactInput, CacheKey, CacheKeyBuilder, CacheKeyInput, CacheStage, InvalidationPlan,
    ManifestChange,
};
pub use contracts::*;
pub use intelligence::{
    apply_scene_plan, identify_clip_candidates, plan_reviewed_timeline, source_range_fingerprint,
    transcript_from_runtime_json, CandidateAnalysis, CandidatePolicy, ScenePlan, ScenePlanRequest,
    TranscriptImportRequest,
};
pub use media::{
    discover_media_runtime, probe_h264_nvenc_runtime, probe_media, validate_caption_cues,
    validate_import_url, CaptionCueInput, CaptionValidation, ImportProvider, MediaChapterProbe,
    MediaError, MediaProbe as RuntimeMediaProbe, MediaRuntimeStatus, MediaStreamProbe,
    MediaToolKind, MediaToolStatus, ValidatedImportUrl,
};
pub use presentation::{
    present_runtime_tools, present_video_project, present_video_project_summary,
};
pub use renderer::{
    build_portrait_command, build_portrait_command_with_layout, build_proxy_command,
    build_thumbnail_command, build_waveform_command, parse_ffmpeg_progress, publish_atomic,
    should_fallback_from_nvenc, sibling_staging_path, terminate_process_group, FfmpegProgress,
    FfmpegProgressParser, FfmpegProgressPhase, PortraitLayout, PublishedArtifact, RenderCommand,
    RenderCommandPlan, RenderProfile, RenderWorkloadClass, VideoEncoder,
};
pub use scheduler::{
    AdmissionBlock, AdmissionOutcome, ResourceCapacity, ResourceClass, ResourceLease,
    ResourceRequest, ResourceScheduler, ResourceUsage, RTX_4080_LAPTOP_VRAM_MB,
};
pub use service::{
    CreateVideoProjectRequest as ServiceCreateVideoProjectRequest, LinkImportRequest, LinkPreview,
    LinkRightsRequest, LocalImportRequest, PortraitRenderRequest, PortraitSourceLayout,
    ProgressCallback, PublishPackageRequest, QueuedVideoJob, ReviseVideoManifestRequest,
    ServiceResult, VideoJobResult, VideoServiceError, VideoServiceProgress, VideoStudioService,
};
pub use timeline::{
    frame_index_at, frame_time_us, map_source_to_timeline, map_timeline_to_source, partition_track,
    quantize_range_outward, quantize_to_frame, source_clock_partition, FramePoint, FrameRange,
    QuantizeMode, SourceClockSpan, SourceClockSpanKind, TimelineSpan, TimelineSpanKind,
};
