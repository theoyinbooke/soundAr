//! Shared, renderer-independent contracts for soundAr Video Studio.
//!
//! The eventual `VideoStudioService` should accept and return these types from every
//! surface (Tauri commands, Codex tools, and any future CLI/HTTP adapter). Media execution,
//! persistence, and transport deliberately sit outside this module.

pub mod assembly;
pub mod cache;
pub mod cast;
pub mod contracts;
pub mod dialogue;
pub mod editor;
pub mod format;
pub mod intelligence;
pub mod lexicon;
pub mod listening;
pub mod media;
pub mod media_server;
pub mod performance;
pub mod presentation;
pub mod quality;
pub mod release;
pub mod renderer;
pub mod scheduler;
pub mod score;
pub mod service;
pub mod sound;
pub mod timeline;
pub mod visuals;

pub use assembly::{
    build_ass_document, build_timeline_render_plan, plan_caption_preview_pages,
    present_caption_presets, write_ass_document_atomic, AssemblyOptions, CaptionPreviewPage,
    CaptionPreviewWord, CaptionTheme,
};
pub use cache::{
    CacheArtifactInput, CacheKey, CacheKeyBuilder, CacheKeyInput, CacheStage, InvalidationPlan,
    ManifestChange,
};
pub use cast::{
    index_cast_by_name, parse_dialogue_script, CastDelivery, CastMember, DialogueTurn, ParsedTurn,
    MAX_CAST_MEMBERS, MAX_DIALOGUE_TURNS, MAX_DIRECTION_BYTES, MAX_SCRIPT_BYTES,
    MAX_TURN_TEXT_BYTES,
};
pub use contracts::*;
pub use dialogue::{apply_dialogue_script, AppliedDialogueScript, DialogueScriptRequest};
pub use editor::{
    apply_timeline_edit, AppliedVideoTimelineEdit, VideoTimelineChangeReceipt,
    VideoTimelineEditRequest, VideoTimelineOperation,
};
pub use format::{
    instantiate_format, materialize_format_cues, CueTemplate, FormatOrigin, ShowFormat,
    MAX_SHOW_FORMATS,
};
pub use intelligence::{
    apply_scene_plan, identify_clip_candidates, plan_reviewed_timeline, source_range_fingerprint,
    transcript_from_runtime_json, CandidateAnalysis, CandidatePolicy, ScenePlan, ScenePlanRequest,
    TranscriptImportRequest,
};
pub use lexicon::{
    apply_lexicon, effective_entries, fingerprint_for_character, lexicon_fingerprint,
    LexiconApplication, LexiconEntry, LexiconMatch, LexiconScope, MAX_LEXICON_ENTRIES,
};
pub use listening::{listen_to_episode, EpisodeListening, GapSummary, ListenedLine, SpeakerShare};
pub use media::{
    discover_media_runtime, local_media_input_args, preflight_import_url_destination,
    probe_h264_nvenc_runtime, probe_media, validate_caption_cues, validate_import_url,
    validate_local_media_source, CaptionCueInput, CaptionValidation, ImportProvider,
    MediaChapterProbe, MediaError, MediaProbe as RuntimeMediaProbe, MediaRuntimeStatus,
    MediaStreamProbe, MediaToolKind, MediaToolStatus, PublicHttpsProxy, ValidatedImportUrl,
    LOCAL_MEDIA_FORMAT_WHITELIST, LOCAL_MEDIA_PROTOCOL_WHITELIST,
};
pub use media_server::LocalMediaServer;
pub use performance::{
    derive_turn_beats, BeatSource, PerformanceClock, TurnBeat, DEFAULT_INTERJECTION_OVERLAP_US,
    MAX_BEAT_US, MAX_OVERLAP_US,
};
pub use presentation::{
    present_runtime_tools, present_video_output, present_video_project,
    present_video_project_summary,
};
pub use quality::{
    build_report, diff_spoken_words, findings_for_caption_drift, findings_for_dead_air,
    findings_for_loudness, findings_for_turn, parse_loudness_analysis, CaptionAlignment,
    LoudnessMeasurement, QcFinding, QcFindingKind, QcReport, QcSeverity, WordDifference,
};
pub use release::{
    episode_chapters, episode_transcript, ffmetadata_chapters, plan_release, ReleaseChapter,
    ReleaseMemberKind, ReleaseMemberPlan, ReleasePlan, TRAILER_MAXIMUM_US, TRAILER_MINIMUM_US,
    TRAILER_TARGET_US,
};
pub use renderer::{
    build_audiogram_command, build_loudness_analysis_command, build_podcast_audio_command,
    build_portrait_command, build_portrait_command_with_layout, build_proxy_command,
    build_thumbnail_command, build_trailer_command, build_waveform_command, parse_ffmpeg_progress,
    publish_atomic, should_fallback_from_nvenc, sibling_staging_path, terminate_process_group,
    FfmpegProgress, FfmpegProgressParser, FfmpegProgressPhase, PortraitLayout, PublishedArtifact,
    RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass, VideoEncoder,
};
pub use scheduler::{
    AdmissionBlock, AdmissionOutcome, ResourceCapacity, ResourceClass, ResourceLease,
    ResourceRequest, ResourceScheduler, ResourceUsage, RTX_4080_LAPTOP_VRAM_MB,
};
pub use score::{
    bed_ducking, fit_cue, CueAnchor, CueFit, CueFitAction, CueRole, MusicCue, CUE_FIT_TOLERANCE_US,
    MAX_MUSIC_CUES,
};
pub(crate) use service::{
    invalidated_stages_for_manifest_changes, manifest_changed_paths, TrustedGeneratedVisual,
};
pub use service::{
    AddVisualAssetRequest, AddVisualAssetResult, AuthorizeVisualSelectionRequest,
    CreateVideoProjectRequest as ServiceCreateVideoProjectRequest, LinkImportRequest, LinkPreview,
    LinkRightsRequest, LocalImportRequest, NarrationReplacement, PortraitRenderRequest,
    PortraitSourceLayout, ProgressCallback, PublishPackageRequest, QueuedVideoJob,
    ReleaseExportResult, ReleaseMemberArtifact, ReplaceNarrationRequest,
    ReviseVideoManifestRequest, ScriptServiceResult, ServiceResult, SharedGpuAdmissionGate,
    SharedGpuAdmissionLease, SharedGpuAdmissionOutcome, SharedGpuAdmissionRequest,
    SharedGpuAdmissionWait, TimelineEditServiceResult, TimelineRenderBatchRequest,
    TimelineRenderProfile, TimelineRenderRequest, VideoJobResult, VideoScriptReceipt,
    VideoScriptRequest, VideoServiceError, VideoServiceProgress, VideoStudioService,
    VisualAssetOrigin, VisualSourceReceipt,
};
pub use sound::{
    assets_matching_tag, SoundAsset, SoundLayer, SoundPlacementKind, MAX_SOUND_ASSETS,
    MAX_SOUND_LAYERS,
};
pub use timeline::{
    frame_index_at, frame_time_us, map_source_endpoint_to_timeline, map_source_to_timeline,
    map_timeline_endpoint_to_source, map_timeline_to_source, partition_track,
    quantize_range_outward, quantize_to_frame, source_clock_partition, FramePoint, FrameRange,
    QuantizeMode, SourceClockSpan, SourceClockSpanKind, TimelineSpan, TimelineSpanKind,
};
pub use visuals::{
    VisualAsset, VisualEasing, VisualFit, VisualLayer, VisualMimeType, VisualMotion,
    MAX_VISUAL_ASSETS, MAX_VISUAL_ASSET_BYTES, MAX_VISUAL_DIMENSION, MAX_VISUAL_LAYERS,
    MAX_VISUAL_PIXELS,
};
