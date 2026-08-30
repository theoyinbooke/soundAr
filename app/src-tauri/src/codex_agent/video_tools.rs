//! Stable Video Studio contracts for Codex dynamic tools.
//!
//! This module deliberately contains no media workflow implementation. The UI and the
//! authenticated Codex assistant both dispatch these operations into the same native adapter,
//! which in turn owns calls into `VideoStudioService`. Keeping parsing, policy and presentation
//! here prevents an assistant-only shadow renderer from emerging over time.

use crate::{video, RuntimeState};
use chrono::{SecondsFormat, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Emitter};

pub(crate) const VIDEO_AGENT_SCHEMA_VERSION: u16 = 1;

pub(crate) type VideoDispatchCallback = fn(
    &RuntimeState,
    VideoAgentOperation,
    Option<video::ProgressCallback>,
) -> Result<VideoAgentResult, VideoAgentToolError>;

#[derive(Clone, Copy)]
pub(crate) struct VideoAgentDispatcher {
    callback: VideoDispatchCallback,
}

impl VideoAgentDispatcher {
    pub(crate) fn new(callback: VideoDispatchCallback) -> Self {
        Self { callback }
    }

    pub(crate) fn dispatch(
        self,
        runtime: &RuntimeState,
        operation: VideoAgentOperation,
        progress: Option<video::ProgressCallback>,
    ) -> Result<VideoAgentResult, VideoAgentToolError> {
        (self.callback)(runtime, operation, progress)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VideoAgentOperationKind {
    VideoRuntimeStatus,
    PreviewLink,
    ImportLink,
    AnalyzeVideo,
    PlanVideo,
    CreateVideoProject,
    ListVideoProjects,
    GetVideoProject,
    EditVideoTimeline,
    WriteVideoScript,
    GenerateCueMusic,
    RegisterGeneratedVisual,
    AddVisualAsset,
    RenderVideoPreview,
    ReviseVideo,
    ExportVideo,
    ExportPublishPackage,
    CancelVideoJob,
    ResumeVideoJob,
}

impl VideoAgentOperationKind {
    #[cfg(test)]
    pub(crate) fn tool_name(self) -> &'static str {
        match self {
            Self::VideoRuntimeStatus => "video_runtime_status",
            Self::PreviewLink => "preview_link",
            Self::ImportLink => "import_link",
            Self::AnalyzeVideo => "analyze_video",
            Self::PlanVideo => "plan_video",
            Self::CreateVideoProject => "create_video_project",
            Self::ListVideoProjects => "list_video_projects",
            Self::GetVideoProject => "get_video_project",
            Self::EditVideoTimeline => "edit_video_timeline",
            Self::WriteVideoScript => "write_video_script",
            Self::GenerateCueMusic => "generate_cue_music",
            Self::RegisterGeneratedVisual => "register_generated_visual",
            Self::AddVisualAsset => "add_visual_asset",
            Self::RenderVideoPreview => "render_video_preview",
            Self::ReviseVideo => "revise_video",
            Self::ExportVideo => "export_video",
            Self::ExportPublishPackage => "export_publish_package",
            Self::CancelVideoJob => "cancel_video_job",
            Self::ResumeVideoJob => "resume_video_job",
        }
    }

    pub(crate) fn phase(self) -> VideoProductionPhase {
        match self {
            Self::VideoRuntimeStatus
            | Self::PreviewLink
            | Self::ImportLink
            | Self::CreateVideoProject
            | Self::RegisterGeneratedVisual => VideoProductionPhase::Source,
            Self::AnalyzeVideo => VideoProductionPhase::Analyze,
            Self::PlanVideo
            | Self::ReviseVideo
            | Self::EditVideoTimeline
            | Self::WriteVideoScript
            | Self::GenerateCueMusic
            | Self::AddVisualAsset => VideoProductionPhase::Review,
            Self::RenderVideoPreview => VideoProductionPhase::Preview,
            Self::ExportVideo | Self::ExportPublishPackage => VideoProductionPhase::Export,
            Self::ListVideoProjects
            | Self::GetVideoProject
            | Self::CancelVideoJob
            | Self::ResumeVideoJob => VideoProductionPhase::Project,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum VideoAgentOperation {
    VideoRuntimeStatus(EmptyRequest),
    PreviewLink(PreviewLinkRequest),
    ImportLink(ImportLinkRequest),
    AnalyzeVideo(AnalyzeVideoRequest),
    PlanVideo(PlanVideoRequest),
    CreateVideoProject(CreateVideoProjectRequest),
    ListVideoProjects(EmptyRequest),
    GetVideoProject(GetVideoProjectRequest),
    EditVideoTimeline(video::VideoTimelineEditRequest),
    WriteVideoScript(video::VideoScriptRequest),
    GenerateCueMusic(GenerateCueMusicRequest),
    RegisterGeneratedVisual(RegisterGeneratedVisualRequest),
    AddVisualAsset(video::AddVisualAssetRequest),
    RenderVideoPreview(RenderVideoPreviewRequest),
    ReviseVideo(ReviseVideoRequest),
    ExportVideo(ExportVideoRequest),
    ExportPublishPackage(ExportPublishPackageRequest),
    CancelVideoJob(VideoJobControlRequest),
    ResumeVideoJob(VideoJobControlRequest),
}

impl VideoAgentOperation {
    pub(crate) fn kind(&self) -> VideoAgentOperationKind {
        match self {
            Self::VideoRuntimeStatus(_) => VideoAgentOperationKind::VideoRuntimeStatus,
            Self::PreviewLink(_) => VideoAgentOperationKind::PreviewLink,
            Self::ImportLink(_) => VideoAgentOperationKind::ImportLink,
            Self::AnalyzeVideo(_) => VideoAgentOperationKind::AnalyzeVideo,
            Self::PlanVideo(_) => VideoAgentOperationKind::PlanVideo,
            Self::CreateVideoProject(_) => VideoAgentOperationKind::CreateVideoProject,
            Self::ListVideoProjects(_) => VideoAgentOperationKind::ListVideoProjects,
            Self::GetVideoProject(_) => VideoAgentOperationKind::GetVideoProject,
            Self::EditVideoTimeline(_) => VideoAgentOperationKind::EditVideoTimeline,
            Self::WriteVideoScript(_) => VideoAgentOperationKind::WriteVideoScript,
            Self::GenerateCueMusic(_) => VideoAgentOperationKind::GenerateCueMusic,
            Self::RegisterGeneratedVisual(_) => VideoAgentOperationKind::RegisterGeneratedVisual,
            Self::AddVisualAsset(_) => VideoAgentOperationKind::AddVisualAsset,
            Self::RenderVideoPreview(_) => VideoAgentOperationKind::RenderVideoPreview,
            Self::ReviseVideo(_) => VideoAgentOperationKind::ReviseVideo,
            Self::ExportVideo(_) => VideoAgentOperationKind::ExportVideo,
            Self::ExportPublishPackage(_) => VideoAgentOperationKind::ExportPublishPackage,
            Self::CancelVideoJob(_) => VideoAgentOperationKind::CancelVideoJob,
            Self::ResumeVideoJob(_) => VideoAgentOperationKind::ResumeVideoJob,
        }
    }

    pub(crate) fn parse(tool: &str, arguments: Value) -> Result<Self, VideoAgentToolError> {
        let arguments = parse_argument_object(arguments)?;
        let operation = match tool {
            "video_runtime_status" => Self::VideoRuntimeStatus(decode(arguments)?),
            "preview_link" => Self::PreviewLink(decode(arguments)?),
            "import_link" => Self::ImportLink(decode(arguments)?),
            "analyze_video" => Self::AnalyzeVideo(decode(arguments)?),
            "plan_video" => Self::PlanVideo(decode(arguments)?),
            "create_video_project" => Self::CreateVideoProject(decode(arguments)?),
            "list_video_projects" => Self::ListVideoProjects(decode(arguments)?),
            "get_video_project" => Self::GetVideoProject(decode(arguments)?),
            "edit_video_timeline" => Self::EditVideoTimeline(decode(arguments)?),
            "write_video_script" => Self::WriteVideoScript(decode(arguments)?),
            "generate_cue_music" => Self::GenerateCueMusic(decode(arguments)?),
            "register_generated_visual" => Self::RegisterGeneratedVisual(decode(arguments)?),
            "add_visual_asset" => Self::AddVisualAsset(decode(arguments)?),
            "render_video_preview" => Self::RenderVideoPreview(decode(arguments)?),
            "revise_video" => Self::ReviseVideo(decode(arguments)?),
            "export_video" => Self::ExportVideo(decode(arguments)?),
            "export_publish_package" => Self::ExportPublishPackage(decode(arguments)?),
            "cancel_video_job" => Self::CancelVideoJob(decode(arguments)?),
            "resume_video_job" => Self::ResumeVideoJob(decode(arguments)?),
            _ => {
                return Err(VideoAgentToolError::new(
                    "video.agent_unknown_tool",
                    format!("Unknown Video Studio tool: {tool}"),
                ))
            }
        };
        operation.validate()?;
        Ok(operation)
    }

    fn validate(&self) -> Result<(), VideoAgentToolError> {
        match self {
            Self::VideoRuntimeStatus(_) | Self::ListVideoProjects(_) => Ok(()),
            Self::PreviewLink(request) => {
                require_text(&request.exact_url, "exact_url")?;
                video::validate_import_url(&request.exact_url)
                    .map(|_| ())
                    .map_err(VideoAgentToolError::from)
            }
            Self::ImportLink(request) => {
                require_text(&request.exact_url, "exact_url")?;
                require_text(&request.rights_confirmation_url, "rights_confirmation_url")?;
                if !request.rights_confirmed || !request.single_source_only {
                    return Err(VideoAgentToolError::approval(
                        "video.rights_required",
                        "Confirm authorization for exactly one source URL before importing",
                        "rights_confirmed",
                    ));
                }
                let validated = video::validate_import_url(&request.exact_url)
                    .map_err(VideoAgentToolError::from)?;
                if validated.is_playlist {
                    return Err(VideoAgentToolError::new(
                        "video.playlist_not_allowed",
                        "Import one exact source URL at a time; playlists are not enabled",
                    ));
                }
                if request.rights_confirmation_url != validated.canonical {
                    return Err(VideoAgentToolError::approval(
                        "video.rights_url_mismatch",
                        "Rights confirmation must match the exact canonical source URL",
                        "rights_confirmation_url",
                    )
                    .details(json!({ "expected_url": validated.canonical })));
                }
                Ok(())
            }
            Self::AnalyzeVideo(request) => {
                require_text(&request.project_id, "project_id")?;
                if request
                    .language
                    .as_deref()
                    .is_some_and(|language| language.trim().is_empty() || language.len() > 64)
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "language",
                        "Language hints must be a non-empty BCP-47 tag of 64 characters or fewer",
                    ));
                }
                Ok(())
            }
            Self::PlanVideo(request) => {
                require_text(&request.project_id, "project_id")?;
                if request
                    .creative_brief
                    .as_deref()
                    .is_some_and(|brief| brief.trim().is_empty() || brief.chars().count() > 12_000)
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "creative_brief",
                        "Creative briefs must be 12,000 characters or fewer",
                    ));
                }
                if request
                    .selected_candidate_ids
                    .as_ref()
                    .is_some_and(Vec::is_empty)
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "selected_candidate_ids",
                        "Select at least one candidate or omit the field to use reviewed defaults",
                    ));
                }
                if request
                    .selected_candidate_ids
                    .as_ref()
                    .is_some_and(|ids| ids.len() > 24)
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "selected_candidate_ids",
                        "A scene plan supports at most 24 selected candidates",
                    ));
                }
                if request.selected_candidate_ids.as_ref().is_some_and(|ids| {
                    ids.iter().any(|id| id.trim().is_empty())
                        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
                }) {
                    return Err(VideoAgentToolError::invalid_field(
                        "selected_candidate_ids",
                        "Candidate ids must be non-empty and unique",
                    ));
                }
                Ok(())
            }
            Self::CreateVideoProject(request) => {
                if request.prompt.chars().count() > 12_000 {
                    return Err(VideoAgentToolError::invalid_field(
                        "prompt",
                        "Creative prompts must be 12,000 characters or fewer",
                    ));
                }
                if request.prompt.trim().is_empty()
                    && request
                        .audio_local_path
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    && request
                        .source_project_id
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "prompt",
                        "Describe the video, provide one local soundAr audio path, or select one soundAr project",
                    ));
                }
                if request.audio_local_path.is_some() && request.source_project_id.is_some() {
                    return Err(VideoAgentToolError::new(
                        "video.single_source_required",
                        "Start with one soundAr project or one local audio source",
                    ));
                }
                Ok(())
            }
            Self::GetVideoProject(request) => require_text(&request.project_id, "project_id"),
            Self::EditVideoTimeline(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.base_version_id, "base_version_id")?;
                require_text(&request.operation_id, "operation_id")?;
                if request.operations.is_empty() || request.operations.len() > 100 {
                    return Err(VideoAgentToolError::invalid_field(
                        "operations",
                        "Submit between one and one hundred ordered timeline edits",
                    ));
                }
                Ok(())
            }
            Self::WriteVideoScript(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.base_version_id, "base_version_id")?;
                require_text(&request.operation_id, "operation_id")?;
                require_text(&request.script, "script")?;
                if request.expected_revision < 1 {
                    return Err(VideoAgentToolError::invalid_field(
                        "expected_revision",
                        "Writing a script requires a positive current project revision",
                    ));
                }
                if request.cast.is_empty() || request.cast.len() > video::MAX_CAST_MEMBERS {
                    return Err(VideoAgentToolError::invalid_field(
                        "cast",
                        "Declare between one and thirty-two characters before writing a script",
                    ));
                }
                Ok(())
            }
            Self::GenerateCueMusic(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.cue_id, "cue_id")?;
                Ok(())
            }
            Self::RegisterGeneratedVisual(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.expected_version_id, "expected_version_id")?;
                require_text(&request.generation_id, "generation_id")?;
                if request.expected_revision < 1 {
                    return Err(VideoAgentToolError::invalid_field(
                        "expected_revision",
                        "Generated visual registration requires a positive current project revision",
                    ));
                }
                if let Some(thread_id) = request.thread_id.as_deref() {
                    require_text(thread_id, "thread_id")?;
                }
                Ok(())
            }
            Self::AddVisualAsset(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.expected_version_id, "expected_version_id")?;
                require_text(&request.operation_id, "operation_id")?;
                require_text(&request.actor, "actor")?;
                if request.expected_revision < 1 {
                    return Err(VideoAgentToolError::invalid_field(
                        "expected_revision",
                        "Visual edits require a positive current project revision",
                    ));
                }
                require_text(request.origin.receipt_id(), "origin.receipt_id")?;
                Ok(())
            }
            Self::RenderVideoPreview(request) => {
                require_text(&request.project_id, "project_id")?;
                if let Some(version) = request.version_id.as_deref() {
                    require_text(version, "version_id")?;
                }
                Ok(())
            }
            Self::ReviseVideo(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.instruction, "instruction")?;
                require_text(&request.base_version_id, "base_version_id")?;
                if request.instruction.chars().count() > 4_000 {
                    return Err(VideoAgentToolError::invalid_field(
                        "instruction",
                        "Revision instructions must be 4,000 characters or fewer",
                    ));
                }
                if let Some(patch) = &request.scene_patch {
                    patch.validate()?;
                }
                Ok(())
            }
            Self::ExportVideo(request) => {
                require_text(&request.project_id, "project_id")?;
                require_text(&request.version_id, "version_id")?;
                if request.format != "mp4" || request.profile != "final" {
                    return Err(VideoAgentToolError::invalid_field(
                        "profile",
                        "Final Video Studio export uses the MP4 final profile",
                    ));
                }
                require_confirmation(request.user_confirmed, "export_video")?;
                if !(1..=8).contains(&request.variations.unwrap_or(1)) {
                    return Err(VideoAgentToolError::invalid_field(
                        "variations",
                        "Render between one and eight variations",
                    ));
                }
                Ok(())
            }
            Self::ExportPublishPackage(request) => {
                require_text(&request.project_id, "project_id")?;
                if request
                    .destination_dir
                    .as_deref()
                    .is_some_and(|path| path.trim().is_empty())
                {
                    return Err(VideoAgentToolError::invalid_field(
                        "destination_dir",
                        "The optional destination directory must not be empty",
                    ));
                }
                require_confirmation(request.user_confirmed, "export_publish_package")
            }
            Self::CancelVideoJob(request) => {
                require_text(&request.job_id, "job_id")?;
                require_confirmation(request.user_confirmed, "cancel_video_job")
            }
            Self::ResumeVideoJob(request) => require_text(&request.job_id, "job_id"),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyRequest {}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterGeneratedVisualRequest {
    pub project_id: String,
    pub expected_revision: i64,
    pub expected_version_id: String,
    pub generation_id: String,
    /// Required by the standalone headless CLI so it can verify the item via Codex thread/read.
    /// In-app dispatch binds the authenticated app-server thread and rejects a different value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviewLinkRequest {
    pub exact_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportLinkRequest {
    pub exact_url: String,
    pub rights_confirmed: bool,
    pub rights_confirmation_url: String,
    pub single_source_only: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzeVideoRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanVideoRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_candidate_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creative_brief: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateVideoProjectRequest {
    #[serde(default)]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_project_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerateCueMusicRequest {
    pub(crate) project_id: String,
    pub(crate) cue_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetVideoProjectRequest {
    pub project_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderVideoPreviewRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_rect: Option<video::NormalizedRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captions_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_bounds: Option<video::NormalizedRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl ScenePatch {
    fn validate(&self) -> Result<(), VideoAgentToolError> {
        if self
            .layout
            .as_deref()
            .is_some_and(|value| !matches!(value, "portrait" | "landscape" | "square"))
        {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.layout",
                "Layout must be portrait, landscape, or square",
            ));
        }
        if self.crop_mode.as_deref() == Some("manual") && self.crop_rect.is_none() {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.crop_rect",
                "Manual crop mode requires an explicit normalized crop rectangle",
            ));
        }
        if self.crop_mode.as_deref() != Some("manual") && self.crop_rect.is_some() {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.crop_rect",
                "A crop rectangle is valid only with manual crop mode",
            ));
        }
        if self.crop_rect.is_some_and(|rect| {
            rect.x_bp < 0
                || rect.y_bp < 0
                || rect.width_bp <= 0
                || rect.height_bp <= 0
                || rect
                    .x_bp
                    .checked_add(rect.width_bp)
                    .is_none_or(|right| right > 10_000)
                || rect
                    .y_bp
                    .checked_add(rect.height_bp)
                    .is_none_or(|bottom| bottom > 10_000)
        }) {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.crop_rect",
                "The normalized crop rectangle must stay within 0..10000 basis points",
            ));
        }
        if self
            .crop_mode
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto-center" | "fit" | "manual"))
        {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.crop_mode",
                "Crop mode must be auto-center, fit, or manual",
            ));
        }
        if let Some(value) = self.caption_style.as_deref() {
            video::CaptionPresetId::parse(value).map_err(|_| {
                VideoAgentToolError::invalid_field(
                    "scene_patch.caption_style",
                    format!(
                        "Caption style must be {}",
                        video::CaptionPresetId::PUBLIC_IDS.join(", ")
                    ),
                )
            })?;
        }
        if let Some(bounds) = self.caption_bounds {
            video::canonical_caption_bounds(bounds).map_err(|_| {
                VideoAgentToolError::invalid_field(
                    "scene_patch.caption_bounds",
                    "Caption bounds need an in-canvas anchor and dimensions of at least 1600x600 basis points",
                )
            })?;
        }
        for (field, gain) in [
            ("scene_patch.voice_gain_db", self.voice_gain_db),
            ("scene_patch.music_gain_db", self.music_gain_db),
        ] {
            if gain.is_some_and(|value| !value.is_finite() || !(-60.0..=12.0).contains(&value)) {
                return Err(VideoAgentToolError::invalid_field(
                    field,
                    "Audio gain must be between -60 dB and +12 dB",
                ));
            }
        }
        let voice_route = [
            ("scene_patch.voice_id", self.voice_id.as_deref(), 128_usize),
            ("scene_patch.model_id", self.model_id.as_deref(), 256),
            ("scene_patch.speaker", self.speaker.as_deref(), 128),
            ("scene_patch.language", self.language.as_deref(), 64),
        ];
        let supplied = voice_route
            .iter()
            .filter(|(_, value, _)| value.is_some())
            .count();
        if supplied != 0 && supplied != voice_route.len() {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.voice_id",
                "A voice revision must provide voice_id, model_id, speaker, and language together",
            ));
        }
        for (field, value, maximum) in voice_route {
            if value.is_some_and(|value| value.trim().is_empty() || value.len() > maximum) {
                return Err(VideoAgentToolError::invalid_field(
                    field,
                    "Voice route values must be non-empty and bounded",
                ));
            }
        }
        if self.language.as_deref().is_some_and(|language| {
            !language.split('-').all(|part| {
                !part.is_empty()
                    && part.len() <= 8
                    && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        }) {
            return Err(VideoAgentToolError::invalid_field(
                "scene_patch.language",
                "Language must be a BCP-47-style tag",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviseVideoRequest {
    pub project_id: String,
    pub instruction: String,
    pub base_version_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_patch: Option<ScenePatch>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportVideoRequest {
    pub project_id: String,
    pub version_id: String,
    pub format: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variations: Option<u16>,
    pub user_confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportPublishPackageRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_dir: Option<String>,
    pub user_confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoJobControlRequest {
    pub job_id: String,
    #[serde(default)]
    pub user_confirmed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VideoProductionPhase {
    Source,
    Analyze,
    Review,
    Preview,
    Export,
    Project,
}

impl VideoProductionPhase {
    fn as_progress_phase(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Analyze => "analyze",
            Self::Review => "review",
            Self::Preview => "preview",
            Self::Export => "export",
            Self::Project => "source",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VideoAgentResultStatus {
    Ready,
    Queued,
    Running,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoAgentOutput {
    /// Always place the assembled, playable master here when it exists. Chapter/scene products
    /// remain secondary so the assistant cannot accidentally foreground an intermediate file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_master: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_artifacts: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoAgentResult {
    pub schema_version: u16,
    pub operation: VideoAgentOperationKind,
    pub phase: VideoProductionPhase,
    pub status: VideoAgentResultStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub output: VideoAgentOutput,
}

impl VideoAgentResult {
    pub(crate) fn ready(
        operation: VideoAgentOperationKind,
        summary: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status: VideoAgentResultStatus::Ready,
            summary: summary.into(),
            project_id: None,
            job_id: None,
            output: VideoAgentOutput {
                data: Some(data),
                ..VideoAgentOutput::default()
            },
        }
    }

    pub(crate) fn project_data(
        operation: VideoAgentOperationKind,
        summary: impl Into<String>,
        project_id: String,
        data: Value,
    ) -> Self {
        Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status: VideoAgentResultStatus::Completed,
            summary: summary.into(),
            project_id: Some(project_id),
            job_id: None,
            output: VideoAgentOutput {
                data: Some(data),
                ..VideoAgentOutput::default()
            },
        }
    }

    pub(crate) fn projects(
        operation: VideoAgentOperationKind,
        summary: impl Into<String>,
        projects: Vec<Value>,
    ) -> Self {
        Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status: VideoAgentResultStatus::Ready,
            summary: summary.into(),
            project_id: None,
            job_id: None,
            output: VideoAgentOutput {
                projects,
                ..VideoAgentOutput::default()
            },
        }
    }

    pub(crate) fn project(
        operation: VideoAgentOperationKind,
        status: VideoAgentResultStatus,
        summary: impl Into<String>,
        project: Value,
        job_id: Option<String>,
    ) -> Result<Self, VideoAgentToolError> {
        let project_id = project
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                VideoAgentToolError::new(
                    "video.invalid_project_result",
                    "The shared video dispatcher returned a project without an identifier",
                )
            })?
            .to_string();
        let (final_master, secondary_artifacts) = prominent_artifacts(&project)?;
        Ok(Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status,
            summary: summary.into(),
            project_id: Some(project_id),
            job_id,
            output: VideoAgentOutput {
                final_master,
                project: Some(project),
                secondary_artifacts,
                ..VideoAgentOutput::default()
            },
        })
    }

    pub(crate) fn artifact(
        operation: VideoAgentOperationKind,
        summary: impl Into<String>,
        project_id: String,
        job_id: Option<String>,
        artifact: Value,
    ) -> Result<Self, VideoAgentToolError> {
        validate_artifact(&artifact)?;
        let final_master = (artifact.get("role").and_then(Value::as_str) == Some("master"))
            .then(|| artifact.clone());
        Ok(Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status: VideoAgentResultStatus::Completed,
            summary: summary.into(),
            project_id: Some(project_id),
            job_id,
            output: VideoAgentOutput {
                final_master,
                artifact: Some(artifact),
                ..VideoAgentOutput::default()
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn job(
        operation: VideoAgentOperationKind,
        summary: impl Into<String>,
        project_id: String,
        job_id: String,
    ) -> Self {
        Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status: VideoAgentResultStatus::Queued,
            summary: summary.into(),
            project_id: Some(project_id),
            job_id: Some(job_id),
            output: VideoAgentOutput::default(),
        }
    }

    pub(crate) fn job_state(
        operation: VideoAgentOperationKind,
        status: VideoAgentResultStatus,
        summary: impl Into<String>,
        project_id: Option<String>,
        job_id: String,
        data: Option<Value>,
    ) -> Self {
        Self {
            schema_version: VIDEO_AGENT_SCHEMA_VERSION,
            operation,
            phase: operation.phase(),
            status,
            summary: summary.into(),
            project_id,
            job_id: Some(job_id),
            output: VideoAgentOutput {
                data,
                ..VideoAgentOutput::default()
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoAgentToolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl VideoAgentToolError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            approval_required: false,
            field: None,
            details: None,
        }
    }

    pub(crate) fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub(crate) fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn invalid_field(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new("video.agent_invalid_field", message).field(field)
    }

    fn approval(code: impl Into<String>, message: impl Into<String>, field: &str) -> Self {
        let mut error = Self::new(code, message).field(field);
        error.approval_required = true;
        error
    }

    fn field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

impl std::fmt::Display for VideoAgentToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VideoAgentToolError {}

impl From<String> for VideoAgentToolError {
    fn from(value: String) -> Self {
        let (code, message) = value
            .split_once(':')
            .filter(|(code, _)| code.trim().starts_with("video."))
            .map(|(code, message)| (code.trim(), message.trim()))
            .unwrap_or(("video.agent_operation_failed", value.as_str()));
        Self::new(code, message)
    }
}

impl From<&str> for VideoAgentToolError {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl From<video::VideoServiceError> for VideoAgentToolError {
    fn from(value: video::VideoServiceError) -> Self {
        Self {
            code: value.code,
            message: value.message,
            retryable: value.retryable,
            approval_required: false,
            field: None,
            details: value.details,
        }
    }
}

impl From<video::MediaError> for VideoAgentToolError {
    fn from(value: video::MediaError) -> Self {
        let code = if value.code.starts_with("video.") {
            value.code
        } else {
            format!("video.media.{}", value.code)
        };
        Self {
            code,
            message: value.message,
            retryable: value.retryable,
            approval_required: false,
            field: None,
            details: value
                .detail
                .map(|diagnostic| json!({ "diagnostic": diagnostic })),
        }
    }
}

impl From<video::VideoError> for VideoAgentToolError {
    fn from(value: video::VideoError) -> Self {
        Self {
            code: value.stable_code().to_string(),
            message: value.message,
            retryable: value.retryable,
            approval_required: false,
            field: value.field,
            details: value.details,
        }
    }
}

pub(crate) fn is_video_tool(tool: &str) -> bool {
    operation_kind(tool).is_some()
}

pub(crate) fn operation_kind(tool: &str) -> Option<VideoAgentOperationKind> {
    Some(match tool {
        "video_runtime_status" => VideoAgentOperationKind::VideoRuntimeStatus,
        "preview_link" => VideoAgentOperationKind::PreviewLink,
        "import_link" => VideoAgentOperationKind::ImportLink,
        "analyze_video" => VideoAgentOperationKind::AnalyzeVideo,
        "plan_video" => VideoAgentOperationKind::PlanVideo,
        "create_video_project" => VideoAgentOperationKind::CreateVideoProject,
        "list_video_projects" => VideoAgentOperationKind::ListVideoProjects,
        "get_video_project" => VideoAgentOperationKind::GetVideoProject,
        "edit_video_timeline" => VideoAgentOperationKind::EditVideoTimeline,
        "write_video_script" => VideoAgentOperationKind::WriteVideoScript,
        "generate_cue_music" => VideoAgentOperationKind::GenerateCueMusic,
        "register_generated_visual" => VideoAgentOperationKind::RegisterGeneratedVisual,
        "add_visual_asset" => VideoAgentOperationKind::AddVisualAsset,
        "render_video_preview" => VideoAgentOperationKind::RenderVideoPreview,
        "revise_video" => VideoAgentOperationKind::ReviseVideo,
        "export_video" => VideoAgentOperationKind::ExportVideo,
        "export_publish_package" => VideoAgentOperationKind::ExportPublishPackage,
        "cancel_video_job" => VideoAgentOperationKind::CancelVideoJob,
        "resume_video_job" => VideoAgentOperationKind::ResumeVideoJob,
        _ => return None,
    })
}

pub(crate) fn requires_studio_access(tool: &str) -> bool {
    matches!(
        operation_kind(tool),
        Some(
            VideoAgentOperationKind::ImportLink
                | VideoAgentOperationKind::AnalyzeVideo
                | VideoAgentOperationKind::PlanVideo
                | VideoAgentOperationKind::CreateVideoProject
                | VideoAgentOperationKind::EditVideoTimeline
                | VideoAgentOperationKind::WriteVideoScript
                | VideoAgentOperationKind::GenerateCueMusic
                | VideoAgentOperationKind::RegisterGeneratedVisual
                | VideoAgentOperationKind::AddVisualAsset
                | VideoAgentOperationKind::RenderVideoPreview
                | VideoAgentOperationKind::ReviseVideo
                | VideoAgentOperationKind::ExportVideo
                | VideoAgentOperationKind::ExportPublishPackage
                | VideoAgentOperationKind::CancelVideoJob
                | VideoAgentOperationKind::ResumeVideoJob
        )
    )
}

pub(crate) fn compact_progress_callback(
    app: AppHandle,
    fallback_phase: VideoProductionPhase,
) -> video::ProgressCallback {
    let emitted = Arc::new(Mutex::new(HashSet::<String>::new()));
    Arc::new(move |update| {
        let phase = compact_phase(&update.phase, fallback_phase);
        // At most eleven percentage milestones per high-level phase, plus every playable partial.
        // This keeps the Studio and assistant informative without producing a card per FFmpeg step.
        let bucket = (update.progress.clamp(0.0, 1.0) * 10.0).floor() as u8;
        let key = format!("{}:{phase}:{bucket}", update.job_id);
        let should_emit = update.playable_artifact.is_some()
            || emitted
                .lock()
                .map(|mut emitted| emitted.insert(key))
                .unwrap_or(true);
        if !should_emit {
            return;
        }
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let payload = json!({
            "job": {
                "id": update.job_id,
                "project_id": update.project_id,
                "phase": phase,
                "status": match update.phase.as_str() {
                    "failed" => "failed",
                    "cancelled" => "cancelled",
                    _ if update.progress >= 1.0 || update.phase == "completed" => "completed",
                    _ => "running",
                },
                "progress": update.progress.clamp(0.0, 1.0),
                "title": phase_title(phase),
                "detail": update.message,
                "durable": true,
                "created_at": timestamp,
                "updated_at": timestamp,
            },
            "partial_artifact": update.playable_artifact,
            "metrics": update.metrics,
        });
        app.emit("video-job-progress", &payload).ok();
        app.emit(
            "codex-agent-event",
            json!({ "method": "soundar/video-phase-progress", "params": payload }),
        )
        .ok();
    })
}

pub(crate) fn tool_catalog() -> Vec<Value> {
    vec![
        tool(
            "video_runtime_status",
            "Check local Video Studio dependencies and hardware readiness before promising an ingest, transcription, preview, or final render.",
            object_schema(&[], Map::new()),
        ),
        tool(
            "preview_link",
            "Preview metadata for one exact authorized link without downloading it. Read-only; playlists and bulk sources are rejected.",
            object_schema(
                &["exact_url"],
                properties([( "exact_url", string("One exact HTTP or HTTPS video URL") )]),
            ),
        ),
        tool(
            "import_link",
            "Import one exact link into a durable Video Studio project. Use only after the user explicitly confirms rights for the canonical URL returned by preview_link. Requires Studio or Full access.",
            object_schema(
                &["exact_url", "rights_confirmed", "rights_confirmation_url", "single_source_only"],
                properties([
                    ("exact_url", string("The exact canonical URL returned by preview_link")),
                    ("rights_confirmed", true_schema("True only after explicit user authorization")),
                    ("rights_confirmation_url", string("Must exactly equal exact_url")),
                    ("single_source_only", true_schema("Must remain true; playlists are disabled")),
                ]),
            ),
        ),
        tool(
            "analyze_video",
            "Run durable local source analysis and timestamped transcription while preserving original source-clock gaps. Requires Studio or Full access.",
            object_schema(
                &["project_id"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("language", nullable_string("Optional BCP-47 language hint")),
                ]),
            ),
        ),
        tool(
            "plan_video",
            "Turn reviewed candidates and an optional researched creative brief into the project scene timeline. Requires Studio or Full access.",
            object_schema(
                &["project_id"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("selected_candidate_ids", json!({"type":"array","minItems":1,"maxItems":24,"uniqueItems":true,"items":{"type":"string","minLength":1}})),
                    ("creative_brief", nullable_bounded_string("Optional research, story angle, audience and structure", 12_000)),
                ]),
            ),
        ),
        tool(
            "create_video_project",
            "Create one durable Video Studio project from a prompt, one existing local soundAr audio artifact, or one soundAr project. Requires Studio or Full access.",
            object_schema(
                &["prompt"],
                properties([
                    ("prompt", bounded_string("Creative brief; may be empty only when a source is supplied", 12_000)),
                    ("audio_local_path", nullable_string("Managed local path for one existing soundAr audio artifact")),
                    ("audio_display_name", nullable_string("Display name for the selected audio")),
                    ("source_project_id", nullable_string("Existing soundAr project id")),
                ]),
            ),
        ),
        tool(
            "list_video_projects",
            "List durable Video Studio projects, including final-master availability. Read-only.",
            object_schema(&[], Map::new()),
        ),
        tool(
            "get_video_project",
            "Read one Video Studio project with its reviewed scenes, revision, jobs, and playable artifacts. Read-only.",
            object_schema(&["project_id"], properties([("project_id", string("Video Studio project id"))])),
        ),
        tool(
            "edit_video_timeline",
            "Apply source-clock-safe split, trim, reorder, or exact merge operations, retime the beat before a dialogue turn, set or remove a pronunciation rule, or reposition a visual layer on one immutable project version. Retiming a conversation reassembles it without re-reading any line; changing a pronunciation rule re-reads only the lines that rule governs; a music bed placed on a track always ducks against the speech beneath it; sound-design placements reference audio the user already registered and never introduce new files. Use one stable operation_id for retries. Requires Studio or Full access.",
            timeline_edit_schema(),
        ),
        tool(
            "write_video_script",
            "Declare the cast and write the speaker-attributed script for one project version. Each character is bound to one voice, and each `NAME: line` becomes one durable dialogue turn. Re-applying a script keeps every turn whose words are unchanged, so revising one line re-reads only that line. Use one stable operation_id for retries. Requires Studio or Full access.",
            video_script_schema(),
        ),
        tool(
            "generate_cue_music",
            "Compose one planned music cue with the installed local music model, register the result as managed project media, fit it to the cue's target length, and place it at the cue's anchor. A bed receives its ducking envelope automatically. The cue already declares its direction, length, and anchor, so this takes only the project and the cue. Requires Studio or Full access.",
            object_schema(
                &["project_id", "cue_id"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("cue_id", string("A music cue in this project that has no music yet")),
                    ("model_id", nullable_string("Installed local music model; defaults to the recommended ACE-Step Studio route")),
                ]),
            ),
        ),
        tool(
            "register_generated_visual",
            "Resolve one completed Codex image-generation item through authenticated app-server history, copy and hash its exact output server-side, and return a one-use receipt for add_visual_asset. In headless mode thread_id is required; in-app mode it is bound to the authenticated current thread. Requires Studio or Full access.",
            object_schema(
                &["project_id", "expected_revision", "expected_version_id", "generation_id"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("expected_revision", json!({"type":"integer","minimum":1})),
                    ("expected_version_id", string("Exact current immutable version id")),
                    ("generation_id", string("Codex image-generation item id")),
                    ("thread_id", nullable_string("Required by the standalone headless CLI; optional in-app and rejected if it differs from the authenticated current thread")),
                ]),
            ),
        ),
        tool(
            "add_visual_asset",
            "Claim one trusted one-use visual source receipt and place its exact PNG, JPEG, or WebP bytes as a durable animated layer on the exact project clock. Use register_generated_visual for Codex-generated images; native user selections receive a picker-minted receipt. Requires Studio or Full access.",
            visual_asset_schema(),
        ),
        tool(
            "render_video_preview",
            "Render or reuse a fast low-resolution preview for the requested project version. Returns a durable job/result and playable artifact when complete. Requires Studio or Full access.",
            object_schema(
                &["project_id"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("version_id", nullable_string("Expected project version; omit only to use the current version")),
                ]),
            ),
        ),
        tool(
            "revise_video",
            "Apply conversational feedback to one project version and invalidate only affected stages. Requires Studio or Full access.",
            revise_schema(),
        ),
        tool(
            "export_video",
            "Render and atomically register the final local MP4. Set user_confirmed only when the user explicitly requested this export. Requires Studio or Full access.",
            object_schema(
                &["project_id", "version_id", "format", "profile", "user_confirmed"],
                properties([
                    ("project_id", string("Video Studio project id")),
                    ("version_id", string("Exact version id being approved for export")),
                    ("format", json!({"type":"string","enum":["mp4"]})),
                    ("profile", json!({"type":"string","enum":["final"]})),
                    ("variations", json!({"type":["integer","null"],"minimum":1,"maximum":8})),
                    ("user_confirmed", true_schema("True only when this final export was explicitly requested")),
                ]),
            ),
        ),
        tool(
            "export_publish_package",
            "Create a registered publish package from an existing final master. Set user_confirmed only after an explicit user request. Requires Studio or Full access.",
            object_schema(
                &["project_id", "user_confirmed"],
                properties([
                    ("project_id", string("Video Studio project id with a final master")),
                    ("destination_dir", nullable_string("Optional user-authorized destination directory")),
                    ("user_confirmed", true_schema("True only when package export was explicitly requested")),
                ]),
            ),
        ),
        tool(
            "cancel_video_job",
            "Cancel one durable Video Studio job. Set user_confirmed only after the user requests cancellation. Requires Studio or Full access.",
            object_schema(
                &["job_id", "user_confirmed"],
                properties([
                    ("job_id", string("Durable Video Studio job id")),
                    ("user_confirmed", true_schema("True only after explicit cancellation approval")),
                ]),
            ),
        ),
        tool(
            "resume_video_job",
            "Resume one interrupted or recoverable durable Video Studio job using its persisted request. Requires Studio or Full access.",
            object_schema(&["job_id"], properties([("job_id", string("Durable Video Studio job id"))])),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn object_schema(required: &[&str], properties: Map<String, Value>) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

fn properties<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn string(description: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "description": description })
}

fn bounded_string(description: &str, maximum: usize) -> Value {
    json!({ "type": "string", "maxLength": maximum, "description": description })
}

fn nullable_string(description: &str) -> Value {
    json!({ "type": ["string", "null"], "minLength": 1, "description": description })
}

fn nullable_bounded_string(description: &str, maximum: usize) -> Value {
    json!({ "type": ["string", "null"], "minLength": 1, "maxLength": maximum, "description": description })
}

fn true_schema(description: &str) -> Value {
    json!({ "type": "boolean", "const": true, "description": description })
}

fn normalized_visual_bounds_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["x_bp","y_bp","width_bp","height_bp"],
        "properties":{
            "x_bp":{"type":"integer","minimum":0,"maximum":9999},
            "y_bp":{"type":"integer","minimum":0,"maximum":9999},
            "width_bp":{"type":"integer","minimum":1,"maximum":10000},
            "height_bp":{"type":"integer","minimum":1,"maximum":10000}
        }
    })
}

fn visual_range_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["start_us","end_us"],
        "properties":{
            "start_us":{"type":"integer","minimum":0},
            "end_us":{"type":"integer","minimum":1}
        }
    })
}

fn visual_motion_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["start_bounds","end_bounds","start_opacity_milli","end_opacity_milli","start_rotation_milli_degrees","end_rotation_milli_degrees","easing"],
        "properties":{
            "start_bounds":normalized_visual_bounds_schema(),
            "end_bounds":normalized_visual_bounds_schema(),
            "start_opacity_milli":{"type":"integer","minimum":0,"maximum":1000},
            "end_opacity_milli":{"type":"integer","minimum":0,"maximum":1000},
            "start_rotation_milli_degrees":{"type":"integer","const":0},
            "end_rotation_milli_degrees":{"type":"integer","const":0},
            "easing":{"type":"string","enum":["linear","ease_in_out"]}
        }
    })
}

/// The cast is the route: a character names the voice, model, and language that perform every
/// line it speaks, so the agent cannot leave a character's delivery implicit.
fn video_script_schema() -> Value {
    object_schema(
        &[
            "project_id",
            "expected_revision",
            "base_version_id",
            "operation_id",
            "cast",
            "script",
        ],
        properties([
            ("project_id", string("Video Studio project id")),
            ("expected_revision", json!({"type":"integer","minimum":1})),
            (
                "base_version_id",
                string("Exact current immutable version id"),
            ),
            (
                "operation_id",
                string("Stable idempotency key for this exact cast and script"),
            ),
            (
                "cast",
                json!({
                    "type": "array",
                    "minItems": 1,
                    "maxItems": video::MAX_CAST_MEMBERS,
                    "items": {
                        "type": "object",
                        "required": ["id", "name", "display_name", "voice_id", "model_id", "language", "created_at"],
                        "additionalProperties": false,
                        "properties": {
                            "id": {"type": "string", "description": "Stable character id, reused across revisions"},
                            "name": {"type": "string", "description": "Script token, e.g. NARRATOR. Matched case-insensitively"},
                            "display_name": {"type": "string"},
                            "voice_id": {"type": "string", "description": "Installed soundAr voice id"},
                            "model_id": {"type": "string", "description": "Installed speech model id"},
                            "language": {"type": "string", "description": "BCP-47 tag, e.g. en-US"},
                            "delivery": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "rate_milli": {"type": "integer", "minimum": 250, "maximum": 4000, "description": "1000 is natural speed"},
                                    "pitch_milli": {"type": "integer", "minimum": -1000, "maximum": 1000},
                                    "energy_milli": {"type": "integer", "minimum": 0, "maximum": 2000}
                                }
                            },
                            "consent_reference_id": {"type": ["string", "null"], "description": "Required when a cloned managed voice performs this character"},
                            "notes": {"type": ["string", "null"]},
                            "created_at": {"type": "string", "description": "UTC RFC3339 timestamp"}
                        }
                    }
                }),
            ),
            (
                "script",
                string(
                    "Speaker-attributed script. Each turn opens with `NAME:` naming a declared cast member; following lines continue it and a blank line closes it. A leading `(direction)` steers performance and is never spoken.",
                ),
            ),
        ]),
    )
}

fn timeline_edit_schema() -> Value {
    object_schema(
        &[
            "project_id",
            "expected_revision",
            "base_version_id",
            "operation_id",
            "operations",
        ],
        properties([
            ("project_id", string("Video Studio project id")),
            (
                "expected_revision",
                json!({"type":"integer","minimum":1,"description":"Exact current project revision"}),
            ),
            (
                "base_version_id",
                string("Exact current immutable version id"),
            ),
            (
                "operation_id",
                string("Stable idempotency identifier for this ordered edit batch"),
            ),
            (
                "operations",
                json!({
                    "type":"array",
                    "minItems":1,
                    "maxItems":100,
                    "items": { "oneOf": timeline_operation_schemas() }
                }),
            ),
        ]),
    )
}

/// Every operation `edit_video_timeline` accepts, one schema per function.
///
/// These are separate functions rather than one literal because `json!` expands recursively and a
/// union this large exceeds the macro's recursion limit when written inline.
fn timeline_operation_schemas() -> Vec<Value> {
    vec![
        split_scene_operation_schema(),
        trim_scene_operation_schema(),
        reorder_scene_operation_schema(),
        merge_scenes_operation_schema(),
        set_turn_beat_operation_schema(),
        clear_turn_beat_operation_schema(),
        set_lexicon_entry_operation_schema(),
        remove_lexicon_entry_operation_schema(),
        set_music_cue_operation_schema(),
        remove_music_cue_operation_schema(),
        place_music_cue_operation_schema(),
        register_sound_asset_operation_schema(),
        remove_sound_asset_operation_schema(),
        set_sound_layer_operation_schema(),
        remove_sound_layer_operation_schema(),
        update_visual_layer_operation_schema(),
    ]
}

fn split_scene_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","scene_id","at_timeline_us"],
                                "properties":{
                                    "type":{"const":"split_scene"},
                                    "scene_id":{"type":"string","minLength":1},
                                    "at_timeline_us":{"type":"integer","minimum":0}
                                }
                            })
}

fn trim_scene_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","scene_id","source_start_us","source_end_us"],
                                "properties":{
                                    "type":{"const":"trim_scene"},
                                    "scene_id":{"type":"string","minLength":1},
                                    "source_start_us":{"type":"integer","minimum":0},
                                    "source_end_us":{"type":"integer","minimum":1}
                                }
                            })
}

fn reorder_scene_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","scene_id","to_index"],
                                "properties":{
                                    "type":{"const":"reorder_scene"},
                                    "scene_id":{"type":"string","minLength":1},
                                    "to_index":{"type":"integer","minimum":0}
                                }
                            })
}

fn merge_scenes_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","first_scene_id","second_scene_id"],
                                "properties":{
                                    "type":{"const":"merge_scenes"},
                                    "first_scene_id":{"type":"string","minLength":1},
                                    "second_scene_id":{"type":"string","minLength":1}
                                }
                            })
}

fn set_turn_beat_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","turn_id","lead_in_us","overlap_us"],
                                "properties":{
                                    "type":{"const":"set_turn_beat"},
                                    "turn_id":{"type":"string","minLength":1},
                                    "lead_in_us":{"type":"integer","minimum":0,"maximum":10000000,"description":"Silence held before this turn. Zero when the turn overlaps instead."},
                                    "overlap_us":{"type":"integer","minimum":0,"maximum":2000000,"description":"How far this turn starts before the previous one ends. Zero when the turn waits instead."}
                                }
                            })
}

fn clear_turn_beat_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","turn_id"],
                                "properties":{
                                    "type":{"const":"clear_turn_beat"},
                                    "turn_id":{"type":"string","minLength":1}
                                }
                            })
}

fn set_lexicon_entry_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","entry"],
                                "properties":{
                                    "type":{"const":"set_lexicon_entry"},
                                    "entry":{
                                        "type":"object",
                                        "additionalProperties":false,
                                        "required":["id","scope","match_text","replacement","matching","created_at"],
                                        "properties":{
                                            "id":{"type":"string","minLength":1},
                                            "scope":{"type":"string","enum":["character","project","global"],"description":"Precedence runs character, then project, then global. A global entry in a project is a snapshot taken when it was imported, so the episode stays reproducible."},
                                            "character_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"Required for character scope and rejected for every other scope"},
                                            "match_text":{"type":"string","minLength":1,"maxLength":200},
                                            "replacement":{"type":"string","minLength":1,"maxLength":400,"description":"Ordinary respelled text, not a phoneme alphabet: engines differ in what notation they accept"},
                                            "matching":{"type":"string","enum":["word","exact"],"description":"word is case-insensitive; exact is case-sensitive, for acronyms"},
                                            "notes":{"oneOf":[{"type":"string"},{"type":"null"}]},
                                            "created_at":{"type":"string","description":"UTC RFC3339 timestamp"}
                                        }
                                    }
                                }
                            })
}

fn remove_lexicon_entry_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","entry_id"],
                                "properties":{
                                    "type":{"const":"remove_lexicon_entry"},
                                    "entry_id":{"type":"string","minLength":1}
                                }
                            })
}

fn set_music_cue_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","cue"],
                                "properties":{
                                    "type":{"const":"set_music_cue"},
                                    "cue":{
                                        "type":"object",
                                        "additionalProperties":false,
                                        "required":["id","role","anchor","target_duration_us","direction","gain_db_milli","fade_in_us","fade_out_us","created_at"],
                                        "properties":{
                                            "id":{"type":"string","minLength":1},
                                            "role":{"type":"string","enum":["sting","bed","transition","outro"],"description":"sting opens, bed sits under dialogue and always ducks, transition covers a cut, outro resolves after the final line"},
                                            "anchor":{
                                                "oneOf":[
                                                    {"type":"object","additionalProperties":false,"required":["kind","scene_id"],"properties":{"kind":{"const":"scene"},"scene_id":{"type":"string","minLength":1}}},
                                                    {"type":"object","additionalProperties":false,"required":["kind","turn_id"],"properties":{"kind":{"const":"turn"},"turn_id":{"type":"string","minLength":1}}},
                                                    {"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"const":"after_final_turn"}}}
                                                ],
                                                "description":"Anchor to a scene or turn so the cue moves when the script is edited. Only an outro may use after_final_turn, and an outro must use it."
                                            },
                                            "target_duration_us":{"type":"integer","minimum":500000,"maximum":900000000,"description":"Ask the local music engine for this length; the rendered result is fitted to it"},
                                            "direction":{"type":"string","minLength":1,"maxLength":2000},
                                            "source_asset_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"The registered soundAr music artifact once generated; null while the cue is only planned"},
                                            "track_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"The audio track carrying this cue. Requires source_asset_id. A bed placed on a track is given its ducking envelope automatically."},
                                            "gain_db_milli":{"type":"integer","minimum":-60000,"maximum":12000},
                                            "fade_in_us":{"type":"integer","minimum":0},
                                            "fade_out_us":{"type":"integer","minimum":0},
                                            "created_at":{"type":"string","description":"UTC RFC3339 timestamp"}
                                        }
                                    }
                                }
                            })
}

fn remove_music_cue_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","cue_id"],
                                "properties":{
                                    "type":{"const":"remove_music_cue"},
                                    "cue_id":{"type":"string","minLength":1}
                                }
                            })
}

fn register_sound_asset_operation_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["type","asset_id","source_asset_id","name","tags"],
        "properties":{
            "type":{"const":"register_sound_asset"},
            "asset_id":{"type":"string","minLength":1},
            "source_asset_id":{"type":"string","minLength":1,"description":"Managed media already imported into this project. Sound design labels imported media; it never names a path on the machine."},
            "name":{"type":"string","minLength":1,"maxLength":256},
            "tags":{"type":"array","maxItems":16,"items":{"type":"string","minLength":1,"maxLength":48},"description":"How this sound is found, e.g. rain, door, room tone. Matched loosely, so written stage directions can locate it."}
        }
    })
}

fn remove_sound_asset_operation_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["type","asset_id"],
        "properties":{
            "type":{"const":"remove_sound_asset"},
            "asset_id":{"type":"string","minLength":1}
        }
    })
}

fn place_music_cue_operation_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["type","cue_id","source_asset_id"],
        "properties":{
            "type":{"const":"place_music_cue"},
            "cue_id":{"type":"string","minLength":1},
            "source_asset_id":{"type":"string","minLength":1,"description":"Registered soundAr music already imported into this project. Prefer generate_cue_music, which composes and places in one durable job."}
        }
    })
}

fn set_sound_layer_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","layer"],
                                "properties":{
                                    "type":{"const":"set_sound_layer"},
                                    "layer":{
                                        "type":"object",
                                        "additionalProperties":false,
                                        "required":["id","asset_id","kind","range","gain_db_milli","fade_in_us","fade_out_us"],
                                        "properties":{
                                            "id":{"type":"string","minLength":1},
                                            "asset_id":{"type":"string","minLength":1,"description":"A sound asset already registered in this project. Placements never introduce new audio."},
                                            "kind":{"type":"string","enum":["one_shot","ambience","room_tone"],"description":"one_shot happens once at a point; ambience runs across a scene span; room_tone runs under a whole scene and is what removes the digital silence between takes"},
                                            "scene_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"Required for ambience and room tone"},
                                            "turn_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"Anchors a one-shot to the line it punctuates so it moves when that line does. Rejected for ambience and room tone."},
                                            "range":visual_range_schema(),
                                            "gain_db_milli":{"type":"integer","minimum":-60000,"maximum":12000,"description":"Room tone must be at or below -18000 so it reads as a room rather than as noise"},
                                            "fade_in_us":{"type":"integer","minimum":0},
                                            "fade_out_us":{"type":"integer","minimum":0},
                                            "loop_to_fill":{"type":"boolean","description":"Repeat the asset across the range. Rejected for a one-shot."},
                                            "duck_under_speech":{"type":"boolean"}
                                        }
                                    }
                                }
                            })
}

fn remove_sound_layer_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","layer_id"],
                                "properties":{
                                    "type":{"const":"remove_sound_layer"},
                                    "layer_id":{"type":"string","minLength":1}
                                }
                            })
}

fn update_visual_layer_operation_schema() -> Value {
    json!({
                                "type":"object",
                                "additionalProperties":false,
                                "required":["type","layer_id","scene_id","range","fit","crop","z_index","motion","transition_in_us","transition_out_us"],
                                "properties":{
                                    "type":{"const":"update_visual_layer"},
                                    "layer_id":{"type":"string","minLength":1},
                                    "scene_id":{"oneOf":[{"type":"string","minLength":1},{"type":"null"}]},
                                    "range":visual_range_schema(),
                                    "fit":{"type":"string","enum":["cover","contain","stretch"]},
                                    "crop":{"oneOf":[normalized_visual_bounds_schema(),{"type":"null"}]},
                                    "z_index":{"type":"integer","minimum":-32768,"maximum":32767},
                                    "motion":visual_motion_schema(),
                                    "transition_in_us":{"type":"integer","minimum":0},
                                    "transition_out_us":{"type":"integer","minimum":0}
                                }
                            })
}

fn visual_asset_schema() -> Value {
    object_schema(
        &[
            "project_id",
            "expected_revision",
            "expected_version_id",
            "operation_id",
            "actor",
            "origin",
            "range",
            "fit",
            "z_index",
            "motion",
            "transition_in_us",
            "transition_out_us",
        ],
        properties([
            ("project_id", string("Video Studio project id")),
            (
                "expected_revision",
                json!({"type":"integer","minimum":1,"description":"Exact current project revision"}),
            ),
            (
                "expected_version_id",
                string("Exact current immutable version id"),
            ),
            (
                "operation_id",
                string("Stable idempotency identifier for this visual import and placement"),
            ),
            ("actor", string("Auditable local actor name")),
            (
                "origin",
                json!({
                    "oneOf":[
                        {
                            "type":"object",
                            "additionalProperties":false,
                            "required":["kind","receipt_id"],
                            "properties":{
                                "kind":{"const":"user_selected"},
                                "receipt_id":{"type":"string","minLength":1,"maxLength":160,"description":"Opaque one-use receipt returned by the native backend file picker"}
                            }
                        },
                        {
                            "type":"object",
                            "additionalProperties":false,
                            "required":["kind","receipt_id"],
                            "properties":{
                                "kind":{"const":"generated_locally"},
                                "receipt_id":{"type":"string","minLength":1,"maxLength":160,"description":"Opaque one-use receipt returned by register_generated_visual"}
                            }
                        }
                    ]
                }),
            ),
            (
                "scene_id",
                nullable_string("Optional scene that owns this visual layer"),
            ),
            ("range", visual_range_schema()),
            (
                "fit",
                json!({"type":"string","enum":["cover","contain","stretch"]}),
            ),
            (
                "crop",
                json!({"oneOf":[normalized_visual_bounds_schema(),{"type":"null"}]}),
            ),
            (
                "z_index",
                json!({"type":"integer","minimum":-32768,"maximum":32767}),
            ),
            ("motion", visual_motion_schema()),
            ("transition_in_us", json!({"type":"integer","minimum":0})),
            ("transition_out_us", json!({"type":"integer","minimum":0})),
        ]),
    )
}

fn revise_schema() -> Value {
    object_schema(
        &["project_id", "instruction", "base_version_id"],
        properties([
            ("project_id", string("Video Studio project id")),
            (
                "instruction",
                bounded_string("Requested creative revision", 4_000),
            ),
            ("base_version_id", string("Exact version being revised")),
            (
                "scene_id",
                nullable_string("Optional scene to revise; omit for project-wide feedback"),
            ),
            (
                "scene_patch",
                json!({
                    "type": ["object", "null"],
                    "additionalProperties": false,
                    "properties": {
                        "layout": {"type":["string","null"],"enum":["portrait","landscape","square",null]},
                        "crop_mode": {"type":["string","null"],"enum":["auto-center","fit","manual",null]},
                        "crop_rect": {
                            "type":["object","null"],
                            "additionalProperties": false,
                            "properties": {
                                "x_bp": {"type":"integer","minimum":0,"maximum":9999},
                                "y_bp": {"type":"integer","minimum":0,"maximum":9999},
                                "width_bp": {"type":"integer","minimum":1,"maximum":10000},
                                "height_bp": {"type":"integer","minimum":1,"maximum":10000}
                            },
                            "required":["x_bp","y_bp","width_bp","height_bp"]
                        },
                        "captions_enabled": {"type":["boolean","null"]},
                        "caption_style": {"type":["string","null"],"enum":["clean-white","calm","kinetic","bold-pop","highlight","karaoke","typewriter","podcast",null]},
                        "caption_bounds": {
                            "type":["object","null"],
                            "additionalProperties": false,
                            "properties": {
                                "x_bp": {"type":"integer","minimum":0,"maximum":9999},
                                "y_bp": {"type":"integer","minimum":0,"maximum":9999},
                                "width_bp": {"type":"integer","minimum":1600,"maximum":2147483647},
                                "height_bp": {"type":"integer","minimum":600,"maximum":2147483647}
                            },
                            "required":["x_bp","y_bp","width_bp","height_bp"],
                            "description":"Per-scene caption rectangle in 0..10000 canvas basis points; edge-crossing resize is canonicalized into frame"
                        },
                        "voice_gain_db": {"type":["number","null"],"minimum":-60,"maximum":12},
                        "music_gain_db": {"type":["number","null"],"minimum":-60,"maximum":12},
                        "voice_id": {"type":["string","null"],"minLength":1,"maxLength":128,"description":"Stable soundAr Voice library id"},
                        "model_id": {"type":["string","null"],"minLength":1,"maxLength":256,"description":"Installed local TTS model id"},
                        "speaker": {"type":["string","null"],"minLength":1,"maxLength":128,"description":"Engine speaker selected for this voice"},
                        "language": {"type":["string","null"],"minLength":1,"maxLength":64,"description":"BCP-47 narration language"}
                    }
                }),
            ),
        ]),
    )
}

fn parse_argument_object(arguments: Value) -> Result<Value, VideoAgentToolError> {
    let value = match arguments {
        Value::String(raw) => serde_json::from_str::<Value>(&raw).map_err(|error| {
            VideoAgentToolError::new(
                "video.agent_invalid_json",
                "Video tool arguments were not valid JSON",
            )
            .details(json!({ "diagnostic": error.to_string() }))
        })?,
        value => value,
    };
    if !value.is_object() {
        return Err(VideoAgentToolError::new(
            "video.agent_invalid_arguments",
            "Video tool arguments must be a JSON object",
        ));
    }
    Ok(value)
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, VideoAgentToolError> {
    serde_json::from_value(value).map_err(|error| {
        VideoAgentToolError::new(
            "video.agent_invalid_arguments",
            "Video tool arguments did not match the stable schema",
        )
        .details(json!({ "diagnostic": error.to_string() }))
    })
}

fn require_text(value: &str, field: &str) -> Result<(), VideoAgentToolError> {
    if value.trim().is_empty() {
        return Err(VideoAgentToolError::invalid_field(
            field,
            format!("{field} must not be empty"),
        ));
    }
    Ok(())
}

fn require_confirmation(value: bool, operation: &str) -> Result<(), VideoAgentToolError> {
    if value {
        Ok(())
    } else {
        Err(VideoAgentToolError::approval(
            "video.approval_required",
            format!("The user must explicitly request {operation} before it runs"),
            "user_confirmed",
        ))
    }
}

fn prominent_artifacts(
    project: &Value,
) -> Result<(Option<Value>, Vec<Value>), VideoAgentToolError> {
    let explicit_master = project
        .get("master")
        .filter(|value| !value.is_null())
        .cloned();
    let artifacts = project
        .pointer("/manifest/artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let master = explicit_master.or_else(|| {
        artifacts
            .iter()
            .find(|artifact| artifact.get("role").and_then(Value::as_str) == Some("master"))
            .cloned()
    });
    if let Some(master) = &master {
        validate_artifact(master)?;
        if master.get("playable").and_then(Value::as_bool) != Some(true) {
            return Err(VideoAgentToolError::new(
                "video.master_not_playable",
                "The final video master must be registered as a playable artifact",
            ));
        }
    }
    let master_id = master
        .as_ref()
        .and_then(|artifact| artifact.get("id"))
        .and_then(Value::as_str);
    let secondary = artifacts
        .into_iter()
        .filter(|artifact| artifact.get("id").and_then(Value::as_str) != master_id)
        .collect();
    Ok((master, secondary))
}

fn validate_artifact(artifact: &Value) -> Result<(), VideoAgentToolError> {
    let object = artifact.as_object().ok_or_else(|| {
        VideoAgentToolError::new(
            "video.invalid_artifact_result",
            "The shared dispatcher returned an invalid artifact",
        )
    })?;
    for field in ["id", "project_id", "role", "mime_type"] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(VideoAgentToolError::new(
                "video.invalid_artifact_result",
                format!("The shared dispatcher artifact is missing {field}"),
            ));
        }
    }
    let has_location = ["url", "local_path"].into_iter().any(|field| {
        object
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    });
    if !has_location {
        return Err(VideoAgentToolError::new(
            "video.artifact_not_registered",
            "The artifact must expose a registered playable or downloadable media location",
        ));
    }
    Ok(())
}

fn compact_phase(phase: &str, fallback: VideoProductionPhase) -> &'static str {
    match phase {
        "validating" | "copying" | "downloading" | "downloaded" | "source_ready" => "source",
        "transcribing" | "analyzing" | "proxy_ready" | "thumbnail_ready" | "waveform_ready" => {
            "analyze"
        }
        "planning" | "reviewing" | "revising" => "review",
        "rendering_preview" | "preview_ready" => "preview",
        "rendering_final" | "publishing" => "export",
        "rendering" if matches!(fallback, VideoProductionPhase::Preview) => "preview",
        "rendering" => "export",
        _ => fallback.as_progress_phase(),
    }
}

fn phase_title(phase: &str) -> &'static str {
    match phase {
        "source" => "Preparing source",
        "analyze" => "Analyzing source",
        "review" => "Planning and revising",
        "preview" => "Rendering preview",
        "export" => "Exporting video",
        _ => "Producing video",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_strict_complete_and_stably_named() {
        let catalog = tool_catalog();
        let names = catalog
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 19);
        assert_eq!(names.iter().copied().collect::<HashSet<_>>().len(), 19);
        for required in [
            "preview_link",
            "import_link",
            "analyze_video",
            "plan_video",
            "create_video_project",
            "list_video_projects",
            "get_video_project",
            "edit_video_timeline",
            "write_video_script",
            "generate_cue_music",
            "register_generated_visual",
            "add_visual_asset",
            "render_video_preview",
            "revise_video",
            "export_video",
            "export_publish_package",
        ] {
            assert!(names.contains(&required), "missing {required}");
        }
        for name in &names {
            let kind = operation_kind(name).unwrap_or_else(|| panic!("unmapped tool {name}"));
            assert_eq!(kind.tool_name(), *name);
        }
        assert!(catalog
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
    }

    #[test]
    fn timeline_edit_tool_exposes_and_parses_the_exact_source_clock_union() {
        let operation = VideoAgentOperation::parse(
            "edit_video_timeline",
            json!({
                "project_id":"project-7",
                "expected_revision":4,
                "base_version_id":"version-4",
                "operation_id":"operation-7",
                "operations":[
                    {"type":"split_scene","scene_id":"scene-1","at_timeline_us":500000},
                    {"type":"reorder_scene","scene_id":"scene-2","to_index":0},
                    {
                        "type":"update_visual_layer",
                        "layer_id":"visual-layer-1",
                        "scene_id":"scene-1",
                        "range":{"start_us":0,"end_us":1000000},
                        "fit":"cover",
                        "crop":null,
                        "z_index":4,
                        "motion":{
                            "start_bounds":{"x_bp":0,"y_bp":0,"width_bp":10000,"height_bp":10000},
                            "end_bounds":{"x_bp":500,"y_bp":500,"width_bp":9000,"height_bp":9000},
                            "start_opacity_milli":1000,
                            "end_opacity_milli":1000,
                            "start_rotation_milli_degrees":0,
                            "end_rotation_milli_degrees":0,
                            "easing":"ease_in_out"
                        },
                        "transition_in_us":150000,
                        "transition_out_us":150000
                    }
                ]
            }),
        )
        .expect("timeline edit request");
        assert_eq!(operation.kind(), VideoAgentOperationKind::EditVideoTimeline);

        let schema = tool_catalog()
            .into_iter()
            .find(|tool| tool["name"] == "edit_video_timeline")
            .expect("timeline edit schema");
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            schema["inputSchema"]["properties"]["operations"]["maxItems"],
            100
        );
        assert_eq!(
            schema["inputSchema"]["properties"]["operations"]["items"]["oneOf"]
                .as_array()
                .map(Vec::len),
            Some(16)
        );

        // Retiming a conversation goes through the same version-bound batch as every other edit.
        let beats = VideoAgentOperation::parse(
            "edit_video_timeline",
            json!({
                "project_id":"project-7",
                "expected_revision":4,
                "base_version_id":"version-4",
                "operation_id":"operation-beats",
                "operations":[
                    {"type":"set_turn_beat","turn_id":"turn-b","lead_in_us":2000000,"overlap_us":0},
                    {"type":"clear_turn_beat","turn_id":"turn-c"}
                ]
            }),
        )
        .expect("beat edit request");
        assert_eq!(beats.kind(), VideoAgentOperationKind::EditVideoTimeline);

        let error = VideoAgentOperation::parse(
            "edit_video_timeline",
            json!({
                "project_id":"project-7",
                "expected_revision":4,
                "base_version_id":"version-4",
                "operation_id":"operation-7",
                "operations":[],
                "unexpected":true
            }),
        )
        .expect_err("unknown fields fail closed");
        assert_eq!(error.code, "video.agent_invalid_arguments");
    }

    #[test]
    fn visual_asset_tools_require_brokered_receipts_and_exact_clock() {
        let registration = VideoAgentOperation::parse(
            "register_generated_visual",
            json!({
                "project_id":"project-visual",
                "expected_revision":3,
                "expected_version_id":"version-3",
                "generation_id":"generation-1",
                "thread_id":"thread-headless"
            }),
        )
        .expect("broker generation registration request");
        assert_eq!(
            registration.kind(),
            VideoAgentOperationKind::RegisterGeneratedVisual
        );
        let operation = VideoAgentOperation::parse(
            "add_visual_asset",
            json!({
                "project_id":"project-visual",
                "expected_revision":3,
                "expected_version_id":"version-3",
                "operation_id":"visual-operation-1",
                "actor":"codex-video-agent",
                "origin":{
                    "kind":"generated_locally",
                    "receipt_id":"visual-generation-receipt-1"
                },
                "scene_id":"scene-1",
                "range":{"start_us":0,"end_us":2000000},
                "fit":"contain",
                "crop":null,
                "z_index":4,
                "motion":{
                    "start_bounds":{"x_bp":1000,"y_bp":2000,"width_bp":8000,"height_bp":4500},
                    "end_bounds":{"x_bp":200,"y_bp":1500,"width_bp":9600,"height_bp":5400},
                    "start_opacity_milli":1000,
                    "end_opacity_milli":1000,
                    "start_rotation_milli_degrees":0,
                    "end_rotation_milli_degrees":0,
                    "easing":"ease_in_out"
                },
                "transition_in_us":150000,
                "transition_out_us":150000
            }),
        )
        .expect("generated visual request");
        assert_eq!(operation.kind(), VideoAgentOperationKind::AddVisualAsset);
        let schema = tool_catalog()
            .into_iter()
            .find(|tool| tool["name"] == "add_visual_asset")
            .expect("visual asset schema");
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
        assert!(schema["inputSchema"]["properties"]
            .get("source_path")
            .is_none());
        assert_eq!(
            schema["inputSchema"]["properties"]["origin"]["oneOf"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        let error = VideoAgentOperation::parse(
            "add_visual_asset",
            json!({
                "project_id":"project-visual",
                "expected_revision":3,
                "expected_version_id":"version-3",
                "operation_id":"visual-operation-2",
                "source_path":"/tmp/user-photo.png",
                "actor":"codex-video-agent",
                "origin":{"kind":"user_selected","user_confirmed":false},
                "range":{"start_us":0,"end_us":1000000},
                "fit":"cover",
                "z_index":1,
                "motion":{
                    "start_bounds":{"x_bp":0,"y_bp":0,"width_bp":10000,"height_bp":10000},
                    "end_bounds":{"x_bp":0,"y_bp":0,"width_bp":10000,"height_bp":10000},
                    "start_opacity_milli":1000,
                    "end_opacity_milli":1000,
                    "start_rotation_milli_degrees":0,
                    "end_rotation_milli_degrees":0,
                    "easing":"linear"
                },
                "transition_in_us":0,
                "transition_out_us":0
            }),
        )
        .expect_err("legacy self-attested approval must fail closed");
        assert_eq!(error.code, "video.agent_invalid_arguments");
        let registration_schema = tool_catalog()
            .into_iter()
            .find(|tool| tool["name"] == "register_generated_visual")
            .expect("registration schema");
        assert!(registration_schema["inputSchema"]["properties"]
            .get("source_path")
            .is_none());
        assert!(registration_schema["inputSchema"]["properties"]
            .get("producer")
            .is_none());
    }

    #[test]
    fn parses_json_strings_and_rejects_unknown_fields() {
        let parsed = VideoAgentOperation::parse(
            "get_video_project",
            Value::String(r#"{"project_id":"project-7"}"#.into()),
        )
        .expect("valid JSON argument string");
        assert_eq!(parsed.kind(), VideoAgentOperationKind::GetVideoProject);

        let error = VideoAgentOperation::parse(
            "get_video_project",
            json!({"project_id":"project-7","surprise":true}),
        )
        .expect_err("unknown fields must fail closed");
        assert_eq!(error.code, "video.agent_invalid_arguments");
    }

    #[test]
    fn voice_revision_route_is_complete_and_exposed_by_the_schema() {
        let operation = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Change the voice",
                "base_version_id": "version-2",
                "scene_id": "scene-opening",
                "scene_patch": {
                    "voice_id": "af_heart",
                    "model_id": "hexgrad/Kokoro-82M",
                    "speaker": "af_heart",
                    "language": "en-US"
                }
            }),
        )
        .expect("complete voice route");
        assert_eq!(operation.kind(), VideoAgentOperationKind::ReviseVideo);

        let error = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Change the voice",
                "base_version_id": "version-2",
                "scene_patch": {"voice_id": "af_heart"}
            }),
        )
        .expect_err("partial voice route");
        assert_eq!(error.code, "video.agent_invalid_field");

        let schema = revise_schema();
        for field in ["voice_id", "model_id", "speaker", "language"] {
            assert!(
                schema
                    .pointer(&format!("/properties/scene_patch/properties/{field}"))
                    .is_some(),
                "missing voice revision schema field {field}"
            );
        }
    }

    #[test]
    fn curated_caption_presets_are_validated_and_exposed_by_the_agent_schema() {
        for style in video::CaptionPresetId::PUBLIC_IDS {
            VideoAgentOperation::parse(
                "revise_video",
                json!({
                    "project_id": "project-7",
                    "instruction": "Change the caption style",
                    "base_version_id": "version-2",
                    "scene_id": "scene-opening",
                    "scene_patch": {"caption_style": style}
                }),
            )
            .unwrap_or_else(|error| panic!("{style} should be accepted: {error:?}"));
        }
        let error = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Use an unknown caption style",
                "base_version_id": "version-2",
                "scene_patch": {"caption_style": "untrusted-template"}
            }),
        )
        .expect_err("unknown caption preset must fail closed");
        assert_eq!(error.code, "video.agent_invalid_field");
        assert_eq!(error.field.as_deref(), Some("scene_patch.caption_style"));

        let schema = revise_schema();
        let values = schema["properties"]["scene_patch"]["properties"]["caption_style"]["enum"]
            .as_array()
            .expect("caption enum");
        for style in video::CaptionPresetId::PUBLIC_IDS {
            assert!(values.iter().any(|value| value.as_str() == Some(style)));
        }
    }

    #[test]
    fn caption_geometry_agent_contract_matches_native_canvas_canonicalization() {
        let operation = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Move and resize the opening captions",
                "base_version_id": "version-2",
                "scene_id": "scene-opening",
                "scene_patch": {
                    "caption_bounds": {
                        "x_bp": 9500,
                        "y_bp": 9200,
                        "width_bp": 2500,
                        "height_bp": 1200
                    }
                }
            }),
        )
        .expect("edge-crossing resize is accepted for canonicalization");
        let VideoAgentOperation::ReviseVideo(request) = operation else {
            panic!("expected revise operation");
        };
        let raw = request
            .scene_patch
            .and_then(|patch| patch.caption_bounds)
            .expect("caption geometry");
        assert_eq!(
            video::canonical_caption_bounds(raw).unwrap(),
            video::NormalizedRect {
                x_bp: 7_500,
                y_bp: 8_800,
                width_bp: 2_500,
                height_bp: 1_200,
            }
        );

        let too_small = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Make captions impossibly small",
                "base_version_id": "version-2",
                "scene_patch": {
                    "caption_bounds": {"x_bp":0,"y_bp":0,"width_bp":1599,"height_bp":600}
                }
            }),
        )
        .expect_err("agent cannot bypass the editor's usable caption minimum");
        assert_eq!(too_small.code, "video.agent_invalid_field");
        assert_eq!(
            too_small.field.as_deref(),
            Some("scene_patch.caption_bounds")
        );

        let schema = revise_schema();
        let geometry = &schema["properties"]["scene_patch"]["properties"]["caption_bounds"];
        assert_eq!(
            geometry["properties"]["width_bp"]["minimum"],
            video::MIN_CAPTION_WIDTH_BP
        );
        assert_eq!(
            geometry["properties"]["height_bp"]["minimum"],
            video::MIN_CAPTION_HEIGHT_BP
        );
        assert_eq!(geometry["additionalProperties"], false);
    }

    #[test]
    fn manual_framing_requires_and_preserves_an_exact_normalized_rectangle() {
        let missing = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Frame the speaker manually",
                "base_version_id": "version-2",
                "scene_id": "scene-opening",
                "scene_patch": {"crop_mode": "manual"}
            }),
        )
        .expect_err("manual framing without coordinates must fail closed");
        assert_eq!(missing.code, "video.agent_invalid_field");
        assert_eq!(missing.field.as_deref(), Some("scene_patch.crop_rect"));

        let parsed = VideoAgentOperation::parse(
            "revise_video",
            json!({
                "project_id": "project-7",
                "instruction": "Frame the speaker manually",
                "base_version_id": "version-2",
                "scene_id": "scene-opening",
                "scene_patch": {
                    "crop_mode": "manual",
                    "crop_rect": {"x_bp": 1750, "y_bp": 500, "width_bp": 5625, "height_bp": 9000}
                }
            }),
        )
        .expect("valid manual frame");
        let VideoAgentOperation::ReviseVideo(request) = parsed else {
            panic!("expected revise operation");
        };
        assert_eq!(
            request.scene_patch.expect("scene patch").crop_rect,
            Some(video::NormalizedRect {
                x_bp: 1_750,
                y_bp: 500,
                width_bp: 5_625,
                height_bp: 9_000,
            })
        );
    }

    #[test]
    fn link_import_requires_exact_per_url_rights_confirmation() {
        let error = VideoAgentOperation::parse(
            "import_link",
            json!({
                "exact_url":"https://www.youtube.com/watch?v=rights-safe",
                "rights_confirmed":true,
                "rights_confirmation_url":"https://www.youtube.com/watch?v=another-source",
                "single_source_only":true
            }),
        )
        .expect_err("mismatched confirmation");
        assert_eq!(error.code, "video.rights_url_mismatch");
        assert!(error.approval_required);

        let error = VideoAgentOperation::parse(
            "import_link",
            json!({
                "exact_url":"https://www.youtube.com/watch?v=rights-safe&list=bulk",
                "rights_confirmed":true,
                "rights_confirmation_url":"https://www.youtube.com/watch?v=rights-safe&list=bulk",
                "single_source_only":true
            }),
        )
        .expect_err("playlist");
        assert_eq!(error.code, "video.media.playlist_not_allowed");
    }

    #[test]
    fn export_and_cancellation_require_explicit_user_confirmation() {
        for (tool, arguments) in [
            (
                "export_video",
                json!({"project_id":"project-1","version_id":"version-2","format":"mp4","profile":"final","user_confirmed":false}),
            ),
            (
                "export_publish_package",
                json!({"project_id":"project-1","user_confirmed":false}),
            ),
            (
                "cancel_video_job",
                json!({"job_id":"job-1","user_confirmed":false}),
            ),
        ] {
            let error = VideoAgentOperation::parse(tool, arguments).expect_err("approval");
            assert_eq!(error.code, "video.approval_required");
            assert!(error.approval_required);
        }
    }

    #[test]
    fn write_policy_leaves_inspection_available_in_read_only_mode() {
        assert!(!requires_studio_access("preview_link"));
        assert!(!requires_studio_access("list_video_projects"));
        assert!(!requires_studio_access("get_video_project"));
        assert!(requires_studio_access("import_link"));
        assert!(requires_studio_access("revise_video"));
        assert!(requires_studio_access("export_video"));
    }

    #[test]
    fn project_result_promotes_only_the_playable_final_master() {
        let project = json!({
            "id":"project-1",
            "name":"Project",
            "master":{
                "id":"master-1","project_id":"project-1","version_id":"version-2",
                "role":"master","mime_type":"video/mp4","playable":true,
                "local_path":"/managed/master.mp4"
            },
            "manifest":{"artifacts":[
                {"id":"preview-1","project_id":"project-1","role":"preview","mime_type":"video/mp4","playable":true},
                {"id":"master-1","project_id":"project-1","role":"master","mime_type":"video/mp4","playable":true}
            ]}
        });
        let result = VideoAgentResult::project(
            VideoAgentOperationKind::ExportVideo,
            VideoAgentResultStatus::Completed,
            "Final master ready",
            project,
            Some("job-1".into()),
        )
        .expect("project result");
        assert_eq!(
            result.output.final_master.as_ref().unwrap()["id"],
            "master-1"
        );
        assert_eq!(result.output.secondary_artifacts.len(), 1);
        assert_eq!(result.output.secondary_artifacts[0]["role"], "preview");
        let serialized = serde_json::to_string(&result.output).expect("serialize");
        assert!(serialized.find("final_master").unwrap() < serialized.find("\"project\"").unwrap());
    }

    #[test]
    fn unplayable_master_fails_closed() {
        let project = json!({
            "id":"project-1",
            "master":{"id":"master-1","project_id":"project-1","role":"master","mime_type":"video/mp4","playable":false,"local_path":"/managed/master.mp4"},
            "manifest":{"artifacts":[]}
        });
        let error = VideoAgentResult::project(
            VideoAgentOperationKind::ExportVideo,
            VideoAgentResultStatus::Completed,
            "bad",
            project,
            None,
        )
        .expect_err("opaque master must not be surfaced");
        assert_eq!(error.code, "video.master_not_playable");
    }

    #[test]
    fn stable_errors_are_machine_readable() {
        let error = VideoAgentToolError::new("video.project_locked", "Project is busy")
            .retryable(true)
            .details(json!({"owner":"job-2"}));
        let value = serde_json::to_value(error).expect("serialize");
        assert_eq!(value["code"], "video.project_locked");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["approval_required"], false);
    }

    #[test]
    fn queued_results_always_expose_durable_project_and_job_ids() {
        let result = VideoAgentResult::job(
            VideoAgentOperationKind::RenderVideoPreview,
            "Preview queued",
            "project-1".into(),
            "job-durable-1".into(),
        );
        assert_eq!(result.status, VideoAgentResultStatus::Queued);
        assert_eq!(result.project_id.as_deref(), Some("project-1"));
        assert_eq!(result.job_id.as_deref(), Some("job-durable-1"));
        assert_eq!(result.phase, VideoProductionPhase::Preview);
    }

    #[test]
    fn low_level_media_events_collapse_into_five_production_phases() {
        assert_eq!(
            compact_phase("downloading", VideoProductionPhase::Export),
            "source"
        );
        assert_eq!(
            compact_phase("transcribing", VideoProductionPhase::Source),
            "analyze"
        );
        assert_eq!(
            compact_phase("revising", VideoProductionPhase::Source),
            "review"
        );
        assert_eq!(
            compact_phase("preview_ready", VideoProductionPhase::Source),
            "preview"
        );
        assert_eq!(
            compact_phase("publishing", VideoProductionPhase::Source),
            "export"
        );
        assert_eq!(
            compact_phase("completed", VideoProductionPhase::Preview),
            "preview"
        );
    }
}
