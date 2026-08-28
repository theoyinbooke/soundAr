//! Thin native adapters for the shared Video Studio service.
//!
//! These commands translate between the canonical microsecond manifest and the compact React
//! projection. Media, rights, persistence, locking, rendering, and job semantics remain owned by
//! `VideoStudioService` and the existing soundAr inference runtime.

#[cfg(test)]
use crate::video::Validate;
use crate::{
    codex_agent::{
        VideoAgentOperation, VideoAgentResult, VideoAgentResultStatus, VideoAgentToolError,
    },
    read_json, video, RuntimeState,
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::process::CommandExt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tauri::Emitter;
use uuid::Uuid;

const VIDEO_COMMAND_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiImportLinkRequest {
    exact_url: String,
    rights_confirmed: bool,
    rights_confirmation_url: String,
    single_source_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiImportLocalRequest {
    local_path: Option<String>,
    display_name: String,
    rights_confirmed: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiCreateProjectRequest {
    prompt: String,
    audio_local_path: Option<String>,
    audio_display_name: Option<String>,
    source_project_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiScenePatch {
    layout: Option<String>,
    crop_mode: Option<String>,
    crop_rect: Option<video::NormalizedRect>,
    captions_enabled: Option<bool>,
    caption_style: Option<String>,
    voice_gain_db: Option<f64>,
    music_gain_db: Option<f64>,
    voice_id: Option<String>,
    model_id: Option<String>,
    speaker: Option<String>,
    language: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiReviseRequest {
    project_id: String,
    instruction: String,
    base_version_id: String,
    scene_id: Option<String>,
    scene_patch: Option<UiScenePatch>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiExportRequest {
    project_id: String,
    version_id: String,
    format: String,
    profile: String,
    variations: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioPathAuthority {
    UserSelected,
    ManagedAgentReference,
}

struct ProjectOperationResult {
    project: Value,
    job_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoiceRevisionSelection {
    voice_id: String,
    model_id: String,
    speaker: String,
    language: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedVoiceRoute {
    selection: VoiceRevisionSelection,
    voice_name: String,
    reference_audio_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableAnalyzeRequest {
    project_id: String,
    source_asset_id: String,
    source_sha256: String,
    model_id: String,
    language: Option<String>,
    expected_revision: i64,
    expected_version_id: String,
    priority: String,
    title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurablePlanRequest {
    project_id: String,
    selected_candidate_ids: Vec<String>,
    creative_brief: Option<String>,
    expected_revision: i64,
    expected_version_id: String,
    priority: String,
    title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableNarrationRevisionRequest {
    project_id: String,
    scene_id: String,
    binding_id: Option<String>,
    script: String,
    script_sha256: String,
    voice_id: String,
    model_id: String,
    speaker: String,
    language: String,
    voice_name: String,
    reference_audio_path: Option<String>,
    expected_revision: i64,
    expected_version_id: String,
    actor: String,
    priority: String,
    title: String,
}

/// Owns the complete prompt → local speech → registered Video Studio import pipeline. The
/// durable parent is created before synthesis and every child is idempotently bound to its id, so
/// restart recovery can adopt History and import work without generating the narration twice.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurablePromptVideoRequest {
    project_id: String,
    prompt: String,
    prompt_sha256: String,
    title: String,
    actor: String,
    model_id: String,
    speaker: String,
    voice_name: String,
    language: String,
    priority: String,
}

/// Single native dispatcher shared by the authenticated Codex dynamic tools and Tauri commands.
/// Every branch delegates to the same service-backed operation helpers below; there is no
/// assistant-only media runner or alternate persistence path.
pub(crate) fn dispatch_video_operation(
    runtime: &RuntimeState,
    operation: VideoAgentOperation,
    progress: Option<video::ProgressCallback>,
) -> Result<VideoAgentResult, VideoAgentToolError> {
    let kind = operation.kind();
    match operation {
        VideoAgentOperation::VideoRuntimeStatus(_) => {
            let data = json!(video::present_runtime_tools(
                &runtime.video.runtime_status(false),
                bundled_whisper_ready(runtime),
            ));
            Ok(VideoAgentResult::ready(
                kind,
                "Video Studio runtime status is ready",
                data,
            ))
        }
        VideoAgentOperation::PreviewLink(request) => {
            let preview = preview_link_value(&runtime.video, &request.exact_url)
                .map_err(VideoAgentToolError::from)?;
            Ok(VideoAgentResult::ready(
                kind,
                "Exact source metadata is ready for rights review",
                preview,
            ))
        }
        VideoAgentOperation::ImportLink(request) => {
            let result = import_link_project(
                runtime,
                UiImportLinkRequest {
                    exact_url: request.exact_url,
                    rights_confirmed: request.rights_confirmed,
                    rights_confirmation_url: request.rights_confirmation_url,
                    single_source_only: request.single_source_only,
                },
                "codex-agent",
                progress,
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Authorized source imported and registered in Video Studio",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::AnalyzeVideo(request) => {
            let result = analyze_project(
                runtime,
                &request.project_id,
                request.language,
                progress.as_ref(),
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Transcript and candidate clips are ready for review",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::PlanVideo(request) => {
            let result = plan_project(
                runtime,
                &request.project_id,
                request.selected_candidate_ids,
                request.creative_brief,
                progress.as_ref(),
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Reviewed scenes are assembled on the canonical timeline",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::CreateVideoProject(request) => {
            let result = create_project_from_request(
                runtime,
                UiCreateProjectRequest {
                    prompt: request.prompt,
                    audio_local_path: request.audio_local_path,
                    audio_display_name: request.audio_display_name,
                    source_project_id: request.source_project_id,
                },
                "codex-agent",
                AudioPathAuthority::ManagedAgentReference,
                progress,
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Video project and its playable source assets are ready",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::ListVideoProjects(_) => {
            let root = runtime.store.video_artifacts_root();
            let projects = runtime
                .video
                .list_projects()
                .map_err(VideoAgentToolError::from)?
                .iter()
                .map(|record| {
                    video::present_video_project_summary(record, &root)
                        .map_err(VideoAgentToolError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(VideoAgentResult::projects(
                kind,
                "Video Studio projects loaded",
                projects,
            ))
        }
        VideoAgentOperation::GetVideoProject(request) => {
            let record = runtime
                .video
                .get_project(&request.project_id)
                .map_err(VideoAgentToolError::from)?;
            let project =
                video::present_video_project(&record, &runtime.store.video_artifacts_root())
                    .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Ready,
                "Video project loaded with playable registered artifacts",
                project,
                None,
            )
        }
        VideoAgentOperation::RenderVideoPreview(request) => {
            let result = render_project(
                runtime,
                &request.project_id,
                request.version_id.as_deref(),
                video::TimelineRenderProfile::Preview,
                1,
                "codex-agent",
                progress,
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Fast timeline preview is playable",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::ReviseVideo(request) => {
            let scene_patch = request.scene_patch.map(|patch| UiScenePatch {
                layout: patch.layout,
                crop_mode: patch.crop_mode,
                crop_rect: patch.crop_rect,
                captions_enabled: patch.captions_enabled,
                caption_style: patch.caption_style,
                voice_gain_db: patch.voice_gain_db,
                music_gain_db: patch.music_gain_db,
                voice_id: patch.voice_id,
                model_id: patch.model_id,
                speaker: patch.speaker,
                language: patch.language,
            });
            let result = revise_project(
                runtime,
                UiReviseRequest {
                    project_id: request.project_id,
                    instruction: request.instruction,
                    base_version_id: request.base_version_id,
                    scene_id: request.scene_id,
                    scene_patch,
                },
                "codex-agent",
                progress.as_ref(),
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Revision applied; only affected production stages were invalidated",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::ExportVideo(request) => {
            let ui_request = UiExportRequest {
                project_id: request.project_id,
                version_id: request.version_id,
                format: request.format,
                profile: request.profile,
                variations: request.variations,
            };
            validate_export_request(&ui_request).map_err(VideoAgentToolError::from)?;
            let result = render_project(
                runtime,
                &ui_request.project_id,
                Some(&ui_request.version_id),
                video::TimelineRenderProfile::Final,
                ui_request.variations.unwrap_or(1),
                "codex-agent",
                progress,
            )
            .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::project(
                kind,
                VideoAgentResultStatus::Completed,
                "Final MP4 export is assembled, playable, and registered",
                result.project,
                result.job_id,
            )
        }
        VideoAgentOperation::ExportPublishPackage(request) => {
            let destination = request
                .destination_dir
                .as_deref()
                .map(authorize_agent_export_destination)
                .transpose()
                .map_err(VideoAgentToolError::from)?;
            let (artifact, job_id) =
                publish_project_package(runtime, &request.project_id, destination, "codex-agent")
                    .map_err(VideoAgentToolError::from)?;
            VideoAgentResult::artifact(
                kind,
                "Checksummed publish ZIP is ready to download",
                request.project_id,
                Some(job_id),
                artifact,
            )
        }
        VideoAgentOperation::CancelVideoJob(request) => {
            let job = runtime
                .store
                .get_job(&request.job_id)
                .map_err(VideoAgentToolError::from)?
                .ok_or_else(|| {
                    VideoAgentToolError::from(
                        "video.job_not_found: The Video Studio task was not found",
                    )
                })?;
            cancel_video_workflow(runtime, &request.job_id).map_err(VideoAgentToolError::from)?;
            Ok(VideoAgentResult::job_state(
                kind,
                VideoAgentResultStatus::Cancelled,
                "Video Studio task cancelled; completed artifacts remain available",
                None,
                request.job_id,
                Some(job),
            ))
        }
        VideoAgentOperation::ResumeVideoJob(request) => {
            let queued = resume_video_workflow(runtime, &request.job_id, progress)
                .map_err(VideoAgentToolError::from)?;
            let data = present_video_job(runtime, &queued).map_err(VideoAgentToolError::from)?;
            Ok(VideoAgentResult::job_state(
                kind,
                VideoAgentResultStatus::Queued,
                "Durable Video Studio task resumed through its owning workflow",
                Some(queued.project_id),
                queued.job_id,
                Some(data),
            ))
        }
    }
}

#[tauri::command]
pub(crate) fn video_runtime_status(
    state: tauri::State<'_, RuntimeState>,
) -> Result<Vec<Value>, String> {
    let status = state.video.runtime_status(false);
    Ok(video::present_runtime_tools(
        &status,
        bundled_whisper_ready(state.inner()),
    ))
}

#[tauri::command]
pub(crate) async fn preview_video_link(
    state: tauri::State<'_, RuntimeState>,
    exact_url: String,
) -> Result<Value, String> {
    let service = Arc::clone(&state.video);
    tauri::async_runtime::spawn_blocking(move || preview_link_value(&service, &exact_url))
        .await
        .map_err(|error| format!("video.worker_failed: Link preview worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn import_video_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    request: UiImportLocalRequest,
) -> Result<Value, String> {
    if !request.rights_confirmed {
        return Err(
            "video.rights_required: Confirm that you are authorized to use this local media".into(),
        );
    }
    let source_path = request
        .local_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or("video.source_required: Choose a local video or audio file")?;
    let runtime = state.video.runtime_status(false);
    let ffprobe = runtime
        .ffprobe
        .path
        .as_deref()
        .filter(|_| runtime.ffprobe.available)
        .ok_or_else(|| {
            "video.ffprobe_unavailable: FFprobe is required to inspect local media".to_string()
        })?;
    let probe = video::probe_media(&source_path, ffprobe).map_err(|error| error.to_string())?;
    let display_name = bounded_name(&request.display_name, "Imported media");
    let service = Arc::clone(&state.video);
    let video_root = state.store.video_artifacts_root();
    let callback = progress_callback(app, "source");
    tauri::async_runtime::spawn_blocking(move || {
        let project = create_empty_project(
            &service,
            &display_name,
            probe.duration_us,
            Some(format!("Import authorized local media: {display_name}")),
            "local-user",
        )?;
        let project_id = value_string(&project, "id")?;
        let queued = service
            .queue_local_import(
                video::LocalImportRequest {
                    project_id: project_id.clone(),
                    source_path,
                    actor: "local-user".into(),
                    title: Some(display_name),
                },
                Some(callback),
            )
            .map_err(service_error)?;
        let result = service
            .wait_for_job(&queued.job_id, &project_id, VIDEO_COMMAND_TIMEOUT)
            .map_err(service_error)?;
        video::present_video_project(&result.project, &video_root)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("video.worker_failed: Local import worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn import_video_link(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    request: UiImportLinkRequest,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "source");
    tauri::async_runtime::spawn_blocking(move || {
        import_link_project(&runtime, request, "local-user", Some(callback))
            .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Link import worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn create_video_project(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    request: UiCreateProjectRequest,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "source");
    tauri::async_runtime::spawn_blocking(move || {
        create_project_from_request(
            &runtime,
            request,
            "local-user",
            AudioPathAuthority::UserSelected,
            Some(callback),
        )
        .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Project creation worker failed: {error}"))?
}

#[tauri::command]
pub(crate) fn list_video_projects(
    state: tauri::State<'_, RuntimeState>,
) -> Result<Vec<Value>, String> {
    let root = state.store.video_artifacts_root();
    state
        .video
        .list_projects()
        .map_err(service_error)?
        .iter()
        .map(|record| {
            video::present_video_project_summary(record, &root).map_err(|error| error.to_string())
        })
        .collect()
}

#[tauri::command]
pub(crate) fn get_video_project(
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
) -> Result<Value, String> {
    let record = state
        .video
        .get_project(&project_id)
        .map_err(service_error)?;
    let mut project = video::present_video_project(&record, &state.store.video_artifacts_root())
        .map_err(|error| error.to_string())?;
    attach_latest_project_job(state.inner(), &project_id, &mut project)?;
    Ok(project)
}

#[tauri::command]
pub(crate) async fn analyze_video(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "analyze");
    tauri::async_runtime::spawn_blocking(move || {
        analyze_project(&runtime, &project_id, None, Some(&callback)).map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Analysis worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn plan_video(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
    selected_candidate_ids: Option<Vec<String>>,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "review");
    tauri::async_runtime::spawn_blocking(move || {
        plan_project(
            &runtime,
            &project_id,
            selected_candidate_ids,
            None,
            Some(&callback),
        )
        .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Planning worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn revise_video(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    request: UiReviseRequest,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "review");
    tauri::async_runtime::spawn_blocking(move || {
        revise_project(&runtime, request, "local-user", Some(&callback))
            .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Revision worker failed: {error}"))?
}

fn revise_project(
    runtime: &RuntimeState,
    request: UiReviseRequest,
    actor: &str,
    progress: Option<&video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    let instruction = request.instruction.trim();
    if instruction.chars().count() > 4_000 {
        return Err(
            "video.invalid_revision: Revision instructions are limited to 4,000 characters".into(),
        );
    }
    if instruction.is_empty() && request.scene_patch.is_none() {
        return Err(
            "video.invalid_revision: Describe the revision or provide scene settings".into(),
        );
    }
    let voice_selection = voice_revision_selection(request.scene_patch.as_ref())?;
    let asks_for_voice = {
        let normalized = instruction.to_ascii_lowercase();
        normalized.contains("change the voice") || normalized.contains("different voice")
    };
    if asks_for_voice && voice_selection.is_none() {
        return Err("video.voice_choice_required: Choose a soundAr voice, installed model, speaker, and language so the speech stage can regenerate only the affected narration".into());
    }
    // Resolve consent and installed-runtime compatibility before mutating the manifest. The
    // durable request below binds the exact reference revision so resume cannot silently switch
    // to a newly uploaded voice sample.
    let mut voice_route = voice_selection
        .map(|selection| resolve_voice_revision_route(runtime, selection))
        .transpose()?;

    let mut record = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;
    let current_version = record
        .pointer("/version/id")
        .and_then(Value::as_str)
        .ok_or("video.invalid_project: Project version is missing")?;
    if current_version != request.base_version_id {
        return Err(format!(
            "video.revision_conflict: This revision targets version {}, but the project is now at {}",
            request.base_version_id, current_version
        ));
    }
    let expected_revision = record
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Project revision is missing")?;
    let mut manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    if voice_route.as_ref().is_some_and(|route| {
        narration_route_matches_manifest(&manifest, request.scene_id.as_deref(), &route.selection)
    }) {
        // Agent callers may repeat the complete route quartet while changing captions, crop, or
        // gain. An identical consent-backed binding is already durable and must not create a
        // redundant synthesis child.
        voice_route = None;
    }
    let original_manifest = manifest.clone();
    let mut changed_paths = BTreeSet::new();
    let mut invalidated = BTreeSet::new();
    if let Some(patch) = request.scene_patch.as_ref() {
        apply_scene_patch(
            &mut manifest,
            request.scene_id.as_deref(),
            patch,
            &mut changed_paths,
            &mut invalidated,
        )?;
    }
    apply_instruction_revision(
        &mut manifest,
        instruction,
        request.scene_id.as_deref(),
        &mut changed_paths,
        &mut invalidated,
    )?;
    changed_paths = manifest_diff_paths(&original_manifest, &manifest)?;
    let has_manifest_edit = !changed_paths.is_empty();
    if !has_manifest_edit && voice_route.is_none() {
        return Err("video.revision_unsupported: That request did not identify a change. Try a caption style, crop, layout, gain, opening-length, or voice adjustment".into());
    }
    let reason = if instruction.is_empty() {
        "Adjust scene settings".to_string()
    } else {
        instruction.to_string()
    };
    if has_manifest_edit {
        invalidated = invalidation_for_manifest_changes(&changed_paths);
        discard_invalidated_render_artifacts(&mut manifest, &invalidated);
        changed_paths = manifest_diff_paths(&original_manifest, &manifest)?;
        let changed_paths = changed_paths.into_iter().collect::<Vec<_>>();
        advance_manifest_revision(
            &mut manifest,
            actor,
            &reason,
            changed_paths.clone(),
            invalidated.clone(),
        )?;
        record = runtime
            .video
            .revise_manifest(video::ReviseVideoManifestRequest {
                project_id: request.project_id.clone(),
                expected_revision,
                manifest,
                actor: actor.into(),
                reason: reason.clone(),
                changed_paths,
                invalidated_stages: invalidated,
                status: Some("ready".into()),
            })
            .map_err(service_error)?;
    }

    let mut job_id = None;
    if let Some(route) = voice_route {
        let current_revision = record
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or("video.invalid_project: Project revision is missing")?;
        let current_version_id = project_version_id(&record)?.to_string();
        let current_manifest: video::VideoProjectManifest = serde_json::from_value(
            record
                .get("manifest")
                .cloned()
                .ok_or("video.invalid_manifest: Project manifest is missing")?,
        )
        .map_err(|error| format!("video.invalid_manifest: {error}"))?;
        let scene = selected_revision_scene(&current_manifest, request.scene_id.as_deref())?;
        let script = scene.script.trim();
        if script.is_empty() {
            return Err("video.narration_script_required: The selected scene has no reviewed script to synthesize".into());
        }
        let binding_id = current_manifest
            .narration_bindings
            .iter()
            .find(|binding| binding.scene_id.as_deref() == Some(scene.id.as_str()))
            .map(|binding| binding.id.clone());
        let durable = DurableNarrationRevisionRequest {
            project_id: request.project_id.clone(),
            scene_id: scene.id.clone(),
            binding_id,
            script: script.to_string(),
            script_sha256: sha256_text(script),
            voice_id: route.selection.voice_id,
            model_id: route.selection.model_id,
            speaker: route.selection.speaker,
            language: route.selection.language,
            voice_name: route.voice_name,
            reference_audio_path: route.reference_audio_path,
            expected_revision: current_revision,
            expected_version_id: current_version_id,
            actor: actor.to_string(),
            priority: "normal".into(),
            title: format!("Regenerate narration · {}", scene.title),
        };
        let durable_value = serde_json::to_value(&durable)
            .map_err(|error| format!("video.invalid_request: {error}"))?;
        let parent_job_id = runtime
            .store
            .create_job("video_regenerate_narration", &durable_value)?;
        record = run_narration_revision_job_guarded(runtime, &parent_job_id, durable, progress)?;
        job_id = Some(parent_job_id);
    }

    let project = video::present_video_project(&record, &runtime.store.video_artifacts_root())
        .map_err(|error| error.to_string())?;
    Ok(ProjectOperationResult { project, job_id })
}

fn run_narration_revision_job_guarded(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurableNarrationRevisionRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_narration_revision_job(runtime, job_id, &request, progress)
    }))
    .unwrap_or_else(|_| {
        Err("video.worker_panicked: The narration revision worker stopped unexpectedly".into())
    });
    if let Err(error) = &result {
        persist_command_job_failure(runtime, job_id, error);
        let checkpoint =
            narration_stage_checkpoint(runtime, &request, job_id).unwrap_or_else(|| json!({}));
        checkpoint_narration_stage(
            runtime,
            job_id,
            &request,
            "failed",
            0.0,
            checkpoint,
            Some(error),
        )
        .ok();
    }
    result
}

fn run_narration_revision_job(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurableNarrationRevisionRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let status = runtime.store.start_job(parent_job_id)?;
    if status == "cancelled" {
        return Err("video.cancelled: Narration regeneration was cancelled".into());
    }
    ensure_command_job_active(runtime, parent_job_id)?;
    if let Some(project) =
        adopt_committed_narration_child(runtime, parent_job_id, request, progress)?
    {
        return Ok(project);
    }
    let record = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;
    ensure_durable_project_expectation(
        &record,
        request.expected_revision,
        &request.expected_version_id,
    )?;
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    let scene = selected_revision_scene(&manifest, Some(&request.scene_id))?;
    if scene.script.trim() != request.script
        || sha256_text(scene.script.trim()) != request.script_sha256
    {
        return Err("video.narration_script_changed: The reviewed scene script changed after this durable narration task was queued".into());
    }
    let resolved = resolve_voice_revision_route(
        runtime,
        VoiceRevisionSelection {
            voice_id: request.voice_id.clone(),
            model_id: request.model_id.clone(),
            speaker: request.speaker.clone(),
            language: request.language.clone(),
        },
    )?;
    if resolved.reference_audio_path != request.reference_audio_path {
        return Err("video.voice_route_changed: The consent-backed voice reference changed after this narration task was queued; choose the voice again to confirm the new reference".into());
    }

    let initial_checkpoint =
        narration_stage_checkpoint(runtime, request, parent_job_id).unwrap_or_else(|| json!({}));
    checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "running",
        0.08,
        initial_checkpoint,
        None,
    )?;
    runtime.store.update_job(parent_job_id, "running", 0.08)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "review",
        0.08,
        "Preparing the selected local voice",
        None,
    );

    let synthesis_request = narration_synthesis_request(request, parent_job_id);
    let (history, synthesis_job_id) = run_narration_synthesis_child(
        runtime,
        parent_job_id,
        request,
        &synthesis_request,
        progress,
    )?;
    validate_narration_history(
        runtime,
        request,
        &synthesis_request,
        &synthesis_job_id,
        &history,
    )?;
    let history_id = history
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("video.tts_failed: The completed narration has no History identity")?
        .to_string();
    let synthesized_checkpoint = json!({
        "synthesis_job_id": synthesis_job_id,
        "history_id": history_id,
    });
    checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "running",
        0.62,
        synthesized_checkpoint.clone(),
        None,
    )?;
    runtime.store.update_job(parent_job_id, "running", 0.62)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "review",
        0.62,
        "Narration is ready; conforming only the affected scene",
        Some(history.clone()),
    );
    ensure_command_job_active(runtime, parent_job_id)?;

    let queued = runtime
        .video
        .queue_narration_replacement(
            narration_replacement_request(request, parent_job_id, &history_id),
            None,
        )
        .map_err(service_error)?;
    let replacement_checkpoint = json!({
        "synthesis_job_id": synthesis_job_id,
        "history_id": history_id,
        "replacement_job_id": queued.job_id,
    });
    if let Err(error) = checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "running",
        0.72,
        replacement_checkpoint,
        None,
    ) {
        runtime.video.cancel_job(&queued.job_id).ok();
        return Err(error);
    }
    if let Err(error) = ensure_command_job_active(runtime, parent_job_id) {
        runtime.video.cancel_job(&queued.job_id).ok();
        return Err(error);
    }
    let completed = runtime
        .video
        .wait_for_job(&queued.job_id, &request.project_id, VIDEO_COMMAND_TIMEOUT)
        .map_err(service_error)?;
    ensure_command_job_active(runtime, parent_job_id)?;
    validate_committed_narration_result(
        &completed.project,
        request,
        &history_id,
        &synthesis_job_id,
    )?;
    checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "completed",
        1.0,
        json!({
            "synthesis_job_id": synthesis_job_id,
            "history_id": history_id,
            "replacement_job_id": queued.job_id,
            "result_version_id": project_version_id(&completed.project)?,
        }),
        None,
    )?;
    complete_narration_parent(runtime, parent_job_id)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "review",
        1.0,
        "The selected scene now uses the new narration",
        None,
    );
    Ok(completed.project)
}

/// Recovers the precise crash window where the service child committed its atomic narration
/// revision but the command-owned parent did not persist its terminal checkpoint. The child is
/// durably keyed by the parent id; validating the resulting canonical binding prevents an
/// unrelated later edit from being mistaken for this narration result.
fn adopt_committed_narration_child(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurableNarrationRevisionRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Option<Value>, String> {
    let Some((child_job_id, mut status)) = runtime
        .store
        .video_child_job(parent_job_id, "video_replace_narration")?
    else {
        return Ok(None);
    };
    let (synthesis_job_id, synthesis_status) = runtime
        .store
        .video_child_job(parent_job_id, "synthesis")?
        .ok_or(
            "video.narration_history_mismatch: The completed replacement has no synthesis child",
        )?;
    if synthesis_status != "completed" {
        return Err(
            "video.narration_history_mismatch: The replacement is not backed by completed synthesis"
                .into(),
        );
    }
    let history = runtime
        .store
        .get_history_by_job_id(&synthesis_job_id)?
        .ok_or(
            "video.narration_history_mismatch: The completed synthesis child has no History item",
        )?;
    let synthesis_request = narration_synthesis_request(request, parent_job_id);
    validate_narration_history(
        runtime,
        request,
        &synthesis_request,
        &synthesis_job_id,
        &history,
    )?;
    let history_id = history
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(
            "video.narration_history_mismatch: The completed synthesis History has no identity",
        )?;
    let mut project = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;

    // A failed child at the original expectation did not publish anything and can continue through
    // the normal path. A failed child after the canonical revision advanced is the precise
    // post-manifest/pre-terminal crash window: prove that the current binding is this exact History
    // take, then rearm the same idempotent child before consulting the stale parent expectation.
    if matches!(status.as_str(), "failed" | "cancelled") {
        if ensure_durable_project_expectation(
            &project,
            request.expected_revision,
            &request.expected_version_id,
        )
        .is_ok()
        {
            return Ok(None);
        }
        validate_committed_narration_result(&project, request, history_id, &synthesis_job_id)?;
        ensure_command_job_active(runtime, parent_job_id)?;
        let queued = runtime
            .video
            .queue_narration_replacement(
                narration_replacement_request(request, parent_job_id, history_id),
                None,
            )
            .map_err(service_error)?;
        if queued.job_id != child_job_id {
            runtime.video.cancel_job(&queued.job_id).ok();
            return Err(
                "video.resume_conflict: Narration recovery selected a different replacement child"
                    .into(),
            );
        }
        if let Err(error) = ensure_command_job_active(runtime, parent_job_id) {
            runtime.video.cancel_job(&child_job_id).ok();
            return Err(error);
        }
        let completed = runtime
            .video
            .wait_for_job(&child_job_id, &request.project_id, VIDEO_COMMAND_TIMEOUT)
            .map_err(service_error)?;
        project = completed.project;
        status = "completed".into();
    } else {
        let deadline = Instant::now() + VIDEO_COMMAND_TIMEOUT;
        while matches!(status.as_str(), "queued" | "preparing" | "running") {
            if let Err(error) = ensure_command_job_active(runtime, parent_job_id) {
                runtime.video.cancel_job(&child_job_id).ok();
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(
                    "video.job_wait_timeout: The narration replacement is still running".into(),
                );
            }
            thread::sleep(Duration::from_millis(75));
            status = runtime
                .store
                .job_status(&child_job_id)?
                .ok_or("video.job_not_found: The durable narration replacement disappeared")?;
        }
        if status == "completed" {
            project = runtime
                .video
                .get_project(&request.project_id)
                .map_err(service_error)?;
        }
    }
    match status.as_str() {
        "completed" => {}
        "failed" => {
            return Err("video.job_failed: The durable narration replacement child failed".into())
        }
        "cancelled" => return Err("video.cancelled: Narration was cancelled".into()),
        state => {
            return Err(format!(
                "video.job_state_invalid: The narration replacement child entered {state}"
            ))
        }
    }

    ensure_command_job_active(runtime, parent_job_id)?;
    validate_committed_narration_result(&project, request, history_id, &synthesis_job_id)?;
    let mut checkpoint =
        narration_stage_checkpoint(runtime, request, parent_job_id).unwrap_or_else(|| json!({}));
    let checkpoint_object = checkpoint
        .as_object_mut()
        .ok_or("video.invalid_checkpoint: The narration recovery checkpoint is not an object")?;
    checkpoint_object.insert(
        "replacement_job_id".into(),
        Value::String(child_job_id.clone()),
    );
    checkpoint_object.insert("synthesis_job_id".into(), Value::String(synthesis_job_id));
    checkpoint_object.insert("history_id".into(), Value::String(history_id.to_string()));
    checkpoint_object.insert(
        "result_version_id".into(),
        Value::String(project_version_id(&project)?.to_string()),
    );
    checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "completed",
        1.0,
        checkpoint,
        None,
    )?;
    complete_narration_parent(runtime, parent_job_id)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "review",
        1.0,
        "Recovered the completed narration revision",
        None,
    );
    Ok(Some(project))
}

fn narration_replacement_request(
    request: &DurableNarrationRevisionRequest,
    parent_job_id: &str,
    history_id: &str,
) -> video::service::ReplaceNarrationRequest {
    video::service::ReplaceNarrationRequest {
        parent_job_id: Some(parent_job_id.to_string()),
        project_id: request.project_id.clone(),
        expected_revision: request.expected_revision,
        expected_version_id: request.expected_version_id.clone(),
        actor: request.actor.clone(),
        replacements: vec![video::service::NarrationReplacement {
            binding_id: request.binding_id.clone(),
            scene_id: Some(request.scene_id.clone()),
            clip_id: None,
            history_id: history_id.to_string(),
            voice_id: request.voice_id.clone(),
            model_id: request.model_id.clone(),
            speaker: request.speaker.clone(),
            language: request.language.clone(),
        }],
    }
}

fn validate_committed_narration_result(
    project: &Value,
    request: &DurableNarrationRevisionRequest,
    expected_history_id: &str,
    expected_generation_job_id: &str,
) -> Result<(), String> {
    let revision = project
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Project revision is missing")?;
    let expected_result_revision = request
        .expected_revision
        .checked_add(1)
        .ok_or("video.invalid_revision: Narration result revision overflow")?;
    if revision != expected_result_revision {
        return Err("video.narration_result_missing: The replacement child completed without advancing the reviewed timeline".into());
    }
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        project
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    if i64::try_from(manifest.revision).ok() != Some(expected_result_revision) {
        return Err(
            "video.narration_result_changed: Narration recovery found a later or incomplete timeline revision"
                .into(),
        );
    }
    let scene = selected_revision_scene(&manifest, Some(&request.scene_id))?;
    if scene.script.trim() != request.script
        || sha256_text(scene.script.trim()) != request.script_sha256
    {
        return Err("video.narration_result_changed: The completed narration no longer matches the reviewed scene script".into());
    }
    let binding = manifest.narration_bindings.iter().find(|binding| {
        request
            .binding_id
            .as_deref()
            .is_some_and(|id| binding.id == id)
            || (request.binding_id.is_none()
                && binding.scene_id.as_deref() == Some(request.scene_id.as_str()))
    });
    let binding = binding.ok_or(
        "video.narration_result_missing: The completed narration binding is not present in the current timeline",
    )?;
    if binding.scene_id.as_deref() != Some(request.scene_id.as_str())
        || binding.voice_id != request.voice_id
        || binding.model_id != request.model_id
        || binding.speaker != request.speaker
        || binding.language != request.language
        || binding.script_sha256 != request.script_sha256
        || binding.history_id != expected_history_id
        || binding.generation_job_id != expected_generation_job_id
    {
        return Err("video.narration_result_changed: The current narration binding no longer matches the durable voice revision".into());
    }
    let artifact = manifest
        .render_artifacts
        .iter()
        .find(|artifact| artifact.id == binding.render_artifact_id)
        .ok_or(
            "video.narration_result_missing: The narration binding artifact is not in the current timeline",
        )?;
    let asset = project
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|asset| asset.get("id").and_then(Value::as_str) == Some(artifact.id.as_str()))
        })
        .ok_or(
            "video.narration_result_missing: The narration artifact has no registered media record",
        )?;
    let provenance = asset.get("provenance").unwrap_or(&Value::Null);
    if asset.get("project_id").and_then(Value::as_str) != Some(request.project_id.as_str())
        || asset.get("kind").and_then(Value::as_str) != Some("speech")
        || asset.get("status").and_then(Value::as_str) != Some("ready")
        || asset.get("content_sha256").and_then(Value::as_str) != Some(artifact.sha256.as_str())
        || provenance.get("history_id").and_then(Value::as_str) != Some(expected_history_id)
        || provenance.get("generation_job_id").and_then(Value::as_str)
            != Some(expected_generation_job_id)
        || provenance.get("voice_id").and_then(Value::as_str) != Some(request.voice_id.as_str())
        || provenance.get("model_id").and_then(Value::as_str) != Some(request.model_id.as_str())
        || provenance.get("speaker").and_then(Value::as_str) != Some(request.speaker.as_str())
        || provenance.get("language").and_then(Value::as_str) != Some(request.language.as_str())
    {
        return Err(
            "video.narration_result_changed: The narration artifact provenance does not match the durable synthesis take"
                .into(),
        );
    }
    Ok(())
}

fn complete_narration_parent(runtime: &RuntimeState, parent_job_id: &str) -> Result<(), String> {
    if runtime.store.complete_job(parent_job_id)? {
        return Ok(());
    }
    if runtime.store.job_status(parent_job_id)?.as_deref() == Some("cancelled") {
        return Err(
            "video.cancelled: Narration regeneration was cancelled before completion".into(),
        );
    }
    Err(
        "video.parent_job_inactive: Narration regeneration lost its durable completion boundary"
            .into(),
    )
}

fn narration_synthesis_request(
    request: &DurableNarrationRevisionRequest,
    parent_job_id: &str,
) -> Value {
    let seed = u32::from_str_radix(&request.script_sha256[..8], 16).unwrap_or(42_817);
    json!({
        "operation": "synthesize",
        "generation_kind": "speech",
        "model_id": request.model_id,
        "text": request.script,
        "input_mode": "text",
        "speaker": request.speaker,
        "language": request.language,
        "reference_audio_path": request.reference_audio_path,
        "speed": 1.0,
        "seed": seed,
        "output_format": "wav",
        "title": request.title,
        "voice_name": request.voice_name,
        "priority": request.priority,
        "video_parent_job_id": parent_job_id,
        "video_project_id": request.project_id,
        "video_scene_id": request.scene_id,
    })
}

fn run_narration_synthesis_child(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurableNarrationRevisionRequest,
    synthesis_request: &Value,
    progress: Option<&video::ProgressCallback>,
) -> Result<(Value, String), String> {
    let idempotency_key = format!("video-narration-synthesis-{parent_job_id}");
    let Some((child_job_id, created)) =
        runtime
            .store
            .create_idempotent_job("synthesis", &idempotency_key, synthesis_request)?
    else {
        return Err("video.resume_conflict: The durable narration synthesis identity is already bound to different inputs".into());
    };
    checkpoint_narration_stage(
        runtime,
        parent_job_id,
        request,
        "running",
        0.12,
        json!({"synthesis_job_id": child_job_id}),
        None,
    )?;
    ensure_command_job_active(runtime, parent_job_id).map_err(|error| {
        runtime.cancel_job(&child_job_id).ok();
        error
    })?;

    let run_child = |child_request: Value| -> Result<Value, String> {
        emit_operation_progress(
            progress,
            parent_job_id,
            &request.project_id,
            "review",
            0.24,
            "Generating narration with the selected local voice",
            None,
        );
        let outcome = runtime
            .request_for_job(child_request.clone(), &child_job_id)
            .and_then(|result| {
                runtime
                    .store
                    .complete_synthesis(&child_job_id, &child_request, &result)
            });
        if let Err(error) = &outcome {
            if runtime.store.job_status(&child_job_id)?.as_deref() != Some("cancelled") {
                runtime.store.fail_job(&child_job_id, error)?;
            }
        }
        outcome.map_err(|error| format!("video.tts_failed: {error}"))
    };

    let history = if created {
        run_child(synthesis_request.clone())?
    } else {
        match runtime.store.job_status(&child_job_id)?.as_deref() {
            Some("completed") => runtime
                .store
                .get_history_by_job_id(&child_job_id)?
                .ok_or("video.tts_failed: The completed narration task has no History artifact")?,
            Some("failed" | "cancelled") => {
                let (_, stored_request) = runtime.store.retry_synthesis_job(&child_job_id)?;
                if stored_request != *synthesis_request {
                    return Err("video.resume_conflict: The stored narration synthesis inputs no longer match the durable parent task".into());
                }
                run_child(stored_request)?
            }
            Some("queued" | "preparing") => run_child(synthesis_request.clone())?,
            Some("running") => wait_for_narration_history(
                runtime,
                parent_job_id,
                &child_job_id,
                VIDEO_COMMAND_TIMEOUT,
            )?,
            Some(status) => {
                return Err(format!(
                    "video.tts_failed: The narration child cannot continue from {status}"
                ))
            }
            None => return Err("video.tts_failed: The narration child task disappeared".into()),
        }
    };
    Ok((history, child_job_id))
}

fn wait_for_narration_history(
    runtime: &RuntimeState,
    parent_job_id: &str,
    child_job_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let started = Instant::now();
    loop {
        ensure_command_job_active(runtime, parent_job_id)?;
        match runtime.store.job_status(child_job_id)?.as_deref() {
            Some("completed") => {
                return runtime
                    .store
                    .get_history_by_job_id(child_job_id)?
                    .ok_or_else(|| {
                        "video.tts_failed: The completed narration task has no History artifact"
                            .into()
                    })
            }
            Some("failed") => {
                return Err("video.tts_failed: The narration synthesis child failed".into())
            }
            Some("cancelled") => return Err("video.cancelled: Narration was cancelled".into()),
            Some("queued" | "preparing" | "running") => {}
            Some(status) => {
                return Err(format!(
                    "video.tts_failed: The narration child entered unsupported state {status}"
                ))
            }
            None => return Err("video.tts_failed: The narration child task disappeared".into()),
        }
        if started.elapsed() >= timeout {
            return Err(
                "video.tts_timeout: Narration synthesis exceeded the local generation deadline"
                    .into(),
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn validate_narration_history(
    runtime: &RuntimeState,
    request: &DurableNarrationRevisionRequest,
    synthesis_request: &Value,
    expected_generation_job_id: &str,
    history: &Value,
) -> Result<(), String> {
    if history.get("generation_kind").and_then(Value::as_str) != Some("speech")
        || history.get("model_id").and_then(Value::as_str) != Some(request.model_id.as_str())
        || history.get("job_id").and_then(Value::as_str) != Some(expected_generation_job_id)
        || !narration_history_artifact_is_intact(history)
    {
        return Err("video.narration_history_mismatch: The generated History artifact is not intact speech from the selected model".into());
    }
    let history_id = history.get("id").and_then(Value::as_str).ok_or(
        "video.narration_history_mismatch: The generated History artifact has no identity",
    )?;
    let stored_request = runtime.store.history_request(history_id)?;
    for field in [
        "model_id",
        "text",
        "speaker",
        "language",
        "reference_audio_path",
        "video_parent_job_id",
        "video_project_id",
        "video_scene_id",
    ] {
        if stored_request.get(field) != synthesis_request.get(field) {
            return Err(format!(
                "video.narration_history_mismatch: The registered History artifact is not bound to the durable narration {field}"
            ));
        }
    }
    let audio_path = history
        .get("audio_path")
        .and_then(Value::as_str)
        .ok_or("video.narration_history_mismatch: The generated History artifact has no audio")?;
    let registered = runtime
        .store
        .get_registered_history_by_audio_path(audio_path)?
        .ok_or(
            "video.narration_history_mismatch: The generated audio is not registered in History",
        )?;
    if registered.get("id").and_then(Value::as_str) != Some(history_id) {
        return Err(
            "video.narration_history_mismatch: The registered audio belongs to different History"
                .into(),
        );
    }
    Ok(())
}

fn narration_history_artifact_is_intact(history: &Value) -> bool {
    matches!(
        history.get("artifact_state").and_then(Value::as_str),
        Some("verified" | "available")
    )
}

fn narration_stage_checkpoint(
    runtime: &RuntimeState,
    request: &DurableNarrationRevisionRequest,
    parent_job_id: &str,
) -> Option<Value> {
    runtime
        .store
        .list_video_stages(&request.project_id)
        .ok()?
        .into_iter()
        .find(|stage| {
            stage.get("version_id").and_then(Value::as_str)
                == Some(request.expected_version_id.as_str())
                && stage.get("stage_key").and_then(Value::as_str) == Some("narration_regeneration")
                && stage.get("scope_key").and_then(Value::as_str) == Some(request.scene_id.as_str())
                && stage.get("job_id").and_then(Value::as_str) == Some(parent_job_id)
        })
        .and_then(|stage| stage.get("checkpoint").cloned())
}

#[allow(clippy::too_many_arguments)]
fn checkpoint_narration_stage(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurableNarrationRevisionRequest,
    status: &str,
    progress: f64,
    checkpoint: Value,
    error: Option<&str>,
) -> Result<(), String> {
    let attempt = runtime
        .store
        .get_job(parent_job_id)?
        .and_then(|job| job.get("attempt").and_then(Value::as_i64))
        .unwrap_or(1);
    let input_sha256 = sha256_text(
        &serde_json::to_string(request)
            .map_err(|error| format!("video.invalid_request: {error}"))?,
    );
    runtime.store.upsert_video_stage(&json!({
        "project_id": request.project_id,
        "version_id": request.expected_version_id,
        "stage_key": "narration_regeneration",
        "scope_key": request.scene_id,
        "job_id": parent_job_id,
        "status": status,
        "resource_class": "heavy",
        "attempt": attempt,
        "progress": progress,
        "input_sha256": input_sha256,
        "checkpoint": checkpoint,
        "error": error.map(|message| json!({"message": message})),
    }))?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn render_video_preview(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "preview");
    tauri::async_runtime::spawn_blocking(move || {
        render_project(
            &runtime,
            &project_id,
            None,
            video::TimelineRenderProfile::Preview,
            1,
            "local-user",
            Some(callback),
        )
        .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Preview render worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn export_video(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    request: UiExportRequest,
) -> Result<Value, String> {
    validate_export_request(&request)?;
    let runtime = state.inner().clone();
    let callback = progress_callback(app, "export");
    tauri::async_runtime::spawn_blocking(move || {
        render_project(
            &runtime,
            &request.project_id,
            Some(&request.version_id),
            video::TimelineRenderProfile::Final,
            request.variations.unwrap_or(1),
            "local-user",
            Some(callback),
        )
        .map(|result| result.project)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Final render worker failed: {error}"))?
}

#[tauri::command]
pub(crate) async fn export_publish_package(
    state: tauri::State<'_, RuntimeState>,
    project_id: String,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        publish_project_package(&runtime, &project_id, None, "local-user").map(|result| result.0)
    })
    .await
    .map_err(|error| format!("video.worker_failed: Package export worker failed: {error}"))?
}

#[tauri::command]
pub(crate) fn cancel_video_job(
    state: tauri::State<'_, RuntimeState>,
    job_id: String,
) -> Result<bool, String> {
    cancel_video_workflow(state.inner(), &job_id)
}

#[tauri::command]
pub(crate) fn resume_video_job(
    app: tauri::AppHandle,
    state: tauri::State<'_, RuntimeState>,
    job_id: String,
) -> Result<Value, String> {
    let kind = state
        .store
        .get_job(&job_id)?
        .and_then(|job| job.get("kind").and_then(Value::as_str).map(str::to_string))
        .ok_or("video.job_not_found: The Video Studio task was not found")?;
    let callback = progress_callback(app, phase_for_job_kind(&kind));
    let queued = resume_video_workflow(state.inner(), &job_id, Some(callback))?;
    present_video_job(state.inner(), &queued)
}

fn cancel_video_workflow(runtime: &RuntimeState, job_id: &str) -> Result<bool, String> {
    let kind = runtime
        .store
        .get_job(job_id)?
        .and_then(|job| job.get("kind").and_then(Value::as_str).map(str::to_string))
        .ok_or("video.job_not_found: The Video Studio task was not found")?;
    if !kind.starts_with("video_") {
        return Err("video.job_mismatch: The selected task is not a Video Studio workflow".into());
    }

    // Signal both execution domains. Service-owned FFmpeg jobs observe the service flag, while
    // analysis may also own an active Python worker that RuntimeState must terminate.
    let service_cancelled = runtime.video.cancel_job(job_id).map_err(service_error)?;
    let inference_cancelled = runtime.cancel_job(job_id)?;
    let mut child_cancelled = false;
    if matches!(
        kind.as_str(),
        "video_regenerate_narration" | "video_create_from_prompt"
    ) {
        for (child_job_id, child_kind) in runtime.store.active_video_child_jobs(job_id)? {
            child_cancelled |= if child_kind == "synthesis" {
                runtime.cancel_job(&child_job_id)?
            } else {
                runtime
                    .video
                    .cancel_job(&child_job_id)
                    .map_err(service_error)?
            };
        }
    }
    Ok(service_cancelled || inference_cancelled || child_cancelled)
}

fn resume_video_workflow(
    runtime: &RuntimeState,
    job_id: &str,
    progress: Option<video::ProgressCallback>,
) -> Result<video::QueuedVideoJob, String> {
    let kind = runtime
        .store
        .get_job(job_id)?
        .and_then(|job| job.get("kind").and_then(Value::as_str).map(str::to_string))
        .ok_or("video.job_not_found: The Video Studio task was not found")?;
    if !kind.starts_with("video_") {
        return Err("video.job_mismatch: The selected task is not a Video Studio workflow".into());
    }
    if !matches!(
        kind.as_str(),
        "video_analyze" | "video_plan" | "video_regenerate_narration" | "video_create_from_prompt"
    ) {
        return runtime
            .video
            .resume_job(job_id, progress)
            .map_err(service_error);
    }

    let (_, durable_request) = runtime
        .store
        .resume_video_job(job_id, &[kind.as_str()])
        .map_err(|error| format!("video.resume_rejected: {error}"))?;
    let project_id = durable_request
        .get("project_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            let message = "video.resume_invalid: The stored Video Studio task has no project";
            runtime.store.fail_job(job_id, message).ok();
            message.to_string()
        })?;
    let runtime_for_worker = runtime.clone();
    let job_id_for_worker = job_id.to_string();
    let kind_for_worker = kind.clone();
    let spawn_result = thread::Builder::new()
        .name(format!("soundar-{}", kind.trim_start_matches("video_")))
        .spawn(move || {
            let outcome = match kind_for_worker.as_str() {
                "video_analyze" => serde_json::from_value::<DurableAnalyzeRequest>(durable_request)
                    .map_err(|error| format!("video.resume_invalid: {error}"))
                    .and_then(|request| {
                        run_analyze_job_guarded(
                            &runtime_for_worker,
                            &job_id_for_worker,
                            request,
                            progress.as_ref(),
                        )
                        .map(|_| ())
                    }),
                "video_plan" => serde_json::from_value::<DurablePlanRequest>(durable_request)
                    .map_err(|error| format!("video.resume_invalid: {error}"))
                    .and_then(|request| {
                        run_plan_job_guarded(
                            &runtime_for_worker,
                            &job_id_for_worker,
                            request,
                            progress.as_ref(),
                        )
                        .map(|_| ())
                    }),
                "video_regenerate_narration" => {
                    serde_json::from_value::<DurableNarrationRevisionRequest>(durable_request)
                        .map_err(|error| format!("video.resume_invalid: {error}"))
                        .and_then(|request| {
                            run_narration_revision_job_guarded(
                                &runtime_for_worker,
                                &job_id_for_worker,
                                request,
                                progress.as_ref(),
                            )
                            .map(|_| ())
                        })
                }
                "video_create_from_prompt" => {
                    serde_json::from_value::<DurablePromptVideoRequest>(durable_request)
                        .map_err(|error| format!("video.resume_invalid: {error}"))
                        .and_then(|request| {
                            run_prompt_video_job_guarded(
                                &runtime_for_worker,
                                &job_id_for_worker,
                                request,
                                progress.as_ref(),
                            )
                            .map(|_| ())
                        })
                }
                _ => unreachable!("command-owned video job kind checked above"),
            };
            if let Err(error) = outcome {
                persist_command_job_failure(&runtime_for_worker, &job_id_for_worker, &error);
            }
        });
    if let Err(error) = spawn_result {
        let message =
            format!("video.worker_failed: Could not resume the Video Studio task: {error}");
        runtime.store.fail_job(job_id, &message).ok();
        return Err(message);
    }
    Ok(video::QueuedVideoJob {
        job_id: job_id.to_string(),
        project_id,
        kind,
    })
}

fn validate_export_request(request: &UiExportRequest) -> Result<(), String> {
    if request.format != "mp4" || request.profile != "final" {
        return Err(
            "video.invalid_export: Final Video Studio exports use the MP4 final profile".into(),
        );
    }
    if !(1..=8).contains(&request.variations.unwrap_or(1)) {
        return Err("video.invalid_export: Render between one and eight variations".into());
    }
    if request.version_id.trim().is_empty() {
        return Err("video.invalid_export: Choose the reviewed project version to export".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_project(
    runtime: &RuntimeState,
    project_id: &str,
    requested_version_id: Option<&str>,
    profile: video::TimelineRenderProfile,
    variations: u16,
    actor: &str,
    progress: Option<video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    if variations == 0 || variations > 8 {
        return Err("video.invalid_export: Render between one and eight variations".into());
    }
    let initial = runtime
        .video
        .get_project(project_id)
        .map_err(service_error)?;
    let initial_version = project_version_id(&initial)?;
    if requested_version_id.is_some_and(|requested| requested != initial_version) {
        return Err(format!(
            "video.revision_conflict: Export targets version {}, but the project is now at {}",
            requested_version_id.unwrap_or_default(),
            initial_version
        ));
    }
    let expected_revision = initial
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Project revision is missing")?;
    let expected_version_id = initial_version.to_string();
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        initial
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    let queued = runtime
        .video
        .queue_timeline_render_batch(
            video::TimelineRenderBatchRequest {
                base: video::TimelineRenderRequest {
                    project_id: project_id.to_string(),
                    expected_revision,
                    expected_version_id,
                    profile,
                    caption_theme: caption_theme(&manifest),
                    portrait_layout: source_layout(&manifest),
                    actor: actor.into(),
                    variation: 0,
                    include_title_cards: true,
                    include_speaker_cards: true,
                    burn_captions: !manifest.captions.is_empty(),
                },
                variations: (0..variations).collect(),
            },
            progress,
        )
        .map_err(service_error)?;
    let completed = runtime
        .video
        .wait_for_job(&queued.job_id, project_id, VIDEO_COMMAND_TIMEOUT)
        .map_err(service_error)?;
    let project =
        video::present_video_project(&completed.project, &runtime.store.video_artifacts_root())
            .map_err(|error| error.to_string())?;
    Ok(ProjectOperationResult {
        project,
        job_id: Some(queued.job_id),
    })
}

fn publish_project_package(
    runtime: &RuntimeState,
    project_id: &str,
    destination_dir: Option<PathBuf>,
    actor: &str,
) -> Result<(Value, String), String> {
    let project = runtime
        .video
        .get_project(project_id)
        .map_err(service_error)?;
    let revision = project
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Project revision is missing")?;
    let version_id = project_version_id(&project)?.to_string();
    let result = runtime
        .video
        .export_publish_package(video::PublishPackageRequest {
            project_id: project_id.into(),
            expected_revision: Some(revision),
            expected_version_id: Some(version_id.clone()),
            destination_dir,
            actor: actor.into(),
        })
        .map_err(service_error)?;
    let output = result
        .get("output")
        .ok_or("video.invalid_result: Package export returned no artifact")?;
    let artifact = video::present_video_output(output, project_id, &version_id)
        .map_err(|error| error.to_string())?;
    let job_id = result
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or("video.invalid_result: Package export returned no durable job")?
        .to_string();
    Ok((artifact, job_id))
}

fn project_version_id(project: &Value) -> Result<&str, String> {
    project
        .pointer("/version/id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "video.invalid_project: Project version is missing".to_string())
}

fn caption_theme(manifest: &video::VideoProjectManifest) -> video::CaptionTheme {
    let style = manifest
        .captions
        .first()
        .map(|caption| caption.style_id.to_ascii_lowercase())
        .unwrap_or_default();
    if style.contains("kinetic") {
        video::CaptionTheme::Kinetic
    } else if style.contains("calm") {
        video::CaptionTheme::Calm
    } else {
        video::CaptionTheme::CleanWhite
    }
}

fn source_layout(manifest: &video::VideoProjectManifest) -> video::PortraitSourceLayout {
    if manifest
        .source_assets
        .iter()
        .any(|source| source.probe.has_video)
    {
        video::PortraitSourceLayout::CenterCrop
    } else {
        video::PortraitSourceLayout::Contain
    }
}

fn phase_for_job_kind(kind: &str) -> &'static str {
    if kind.contains("import") || kind == "video_create_from_prompt" {
        "source"
    } else if kind.contains("plan") || kind.contains("narration") {
        "review"
    } else if kind.contains("preview") {
        "preview"
    } else if kind.contains("render") || kind.contains("package") {
        "export"
    } else {
        "analyze"
    }
}

fn present_video_job(
    runtime: &RuntimeState,
    queued: &video::QueuedVideoJob,
) -> Result<Value, String> {
    let job = runtime
        .store
        .get_job(&queued.job_id)?
        .ok_or("video.job_not_found: The Video Studio task was not found")?;
    Ok(present_stored_video_job(
        &queued.project_id,
        &queued.kind,
        &job,
    ))
}

fn present_stored_video_job(project_id: &str, kind: &str, job: &Value) -> Value {
    let phase = phase_for_job_kind(kind);
    json!({
        "id": job.get("id").and_then(Value::as_str).unwrap_or(""),
        "project_id": project_id,
        "phase": phase,
        "status": job.get("status").and_then(Value::as_str).unwrap_or("queued"),
        "progress": job.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
        "title": phase_title(phase),
        "detail": job.get("error").and_then(Value::as_str).unwrap_or("Resuming durable Video Studio task"),
        "durable": true,
        "created_at": job.get("created_at").and_then(Value::as_str).unwrap_or_else(|| "1970-01-01T00:00:00Z"),
        "updated_at": job.get("updated_at").and_then(Value::as_str).unwrap_or_else(|| "1970-01-01T00:00:00Z"),
        "error": job.get("error"),
    })
}

fn attach_latest_project_job(
    runtime: &RuntimeState,
    project_id: &str,
    project: &mut Value,
) -> Result<(), String> {
    let Some(job) = runtime.store.latest_video_project_job(project_id)? else {
        return Ok(());
    };
    let kind = job
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("video.job_state_invalid: The stored Video Studio task has no kind")?;
    let presented = present_stored_video_job(project_id, kind, &job);
    let object = project
        .as_object_mut()
        .ok_or("video.invalid_project: Presented Video Studio project is not an object")?;
    object.insert("workflow_job".into(), presented.clone());
    if matches!(
        job.get("status").and_then(Value::as_str),
        Some("failed" | "cancelled")
    ) {
        object.insert("recoverable_job".into(), presented);
    }
    Ok(())
}

fn apply_scene_patch(
    manifest: &mut video::VideoProjectManifest,
    requested_scene_id: Option<&str>,
    patch: &UiScenePatch,
    changed_paths: &mut BTreeSet<String>,
    invalidated: &mut BTreeSet<video::RevisionStage>,
) -> Result<(), String> {
    let scene_id = requested_scene_id
        .or_else(|| {
            manifest
                .reviewed_scenes
                .first()
                .map(|scene| scene.id.as_str())
        })
        .ok_or("video.scene_required: Plan at least one scene before revising scene settings")?
        .to_string();
    if !manifest
        .reviewed_scenes
        .iter()
        .any(|scene| scene.id == scene_id)
    {
        return Err(
            "video.scene_not_found: The selected scene is no longer in this version".into(),
        );
    }

    if let Some(layout) = patch.layout.as_deref() {
        set_canvas_layout(manifest, layout)?;
        changed_paths.insert("/layout".into());
        invalidate_visual_render(invalidated);
    }
    if let Some(crop_mode) = patch.crop_mode.as_deref() {
        set_scene_crop(manifest, &scene_id, crop_mode, patch.crop_rect)?;
        changed_paths.insert("/tracks".into());
        changed_paths.insert("/layout/elements".into());
        invalidate_visual_render(invalidated);
    } else if patch.crop_rect.is_some() {
        return Err("video.invalid_crop: A manual crop rectangle requires crop_mode=manual".into());
    }
    if let Some(enabled) = patch.captions_enabled {
        set_scene_captions(manifest, &scene_id, enabled, patch.caption_style.as_deref())?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if let Some(style) = patch.caption_style.as_deref() {
        set_caption_style(manifest, Some(&scene_id), style)?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    }
    if let Some(gain) = patch.voice_gain_db {
        set_mix_gain(manifest, "audio-main", gain)?;
        changed_paths.insert("/audio_mix/tracks/audio-main".into());
        invalidate_audio_render(invalidated);
    }
    if let Some(gain) = patch.music_gain_db {
        set_mix_gain(manifest, "music-main", gain)?;
        changed_paths.insert("/audio_mix/tracks/music-main".into());
        invalidate_audio_render(invalidated);
    }
    Ok(())
}

fn voice_revision_selection(
    patch: Option<&UiScenePatch>,
) -> Result<Option<VoiceRevisionSelection>, String> {
    let Some(patch) = patch else {
        return Ok(None);
    };
    let supplied = [
        patch.voice_id.is_some(),
        patch.model_id.is_some(),
        patch.speaker.is_some(),
        patch.language.is_some(),
    ]
    .into_iter()
    .filter(|value| *value)
    .count();
    if supplied == 0 {
        return Ok(None);
    }
    if supplied != 4 {
        return Err("video.voice_route_incomplete: Choose a voice, installed model, speaker, and language together".into());
    }
    let bounded = |value: &Option<String>, field: &str, maximum: usize| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= maximum)
            .map(str::to_string)
            .ok_or_else(|| {
                format!("video.invalid_voice_route: {field} must be non-empty and at most {maximum} bytes")
            })
    };
    let selection = VoiceRevisionSelection {
        voice_id: bounded(&patch.voice_id, "voice_id", 128)?,
        model_id: bounded(&patch.model_id, "model_id", 256)?,
        speaker: bounded(&patch.speaker, "speaker", 128)?,
        language: bounded(&patch.language, "language", 64)?,
    };
    if !selection.language.split('-').all(|part| {
        !part.is_empty() && part.len() <= 8 && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return Err("video.invalid_voice_route: language must be a BCP-47-style tag".into());
    }
    Ok(Some(selection))
}

fn narration_route_matches_manifest(
    manifest: &video::VideoProjectManifest,
    requested_scene_id: Option<&str>,
    selection: &VoiceRevisionSelection,
) -> bool {
    let Some(scene) = requested_scene_id
        .and_then(|id| manifest.reviewed_scenes.iter().find(|scene| scene.id == id))
        .or_else(|| manifest.reviewed_scenes.first())
    else {
        return false;
    };
    let script_sha256 = sha256_text(scene.script.trim());
    manifest.narration_bindings.iter().any(|binding| {
        binding.scene_id.as_deref() == Some(scene.id.as_str())
            && binding.voice_id == selection.voice_id
            && binding.model_id == selection.model_id
            && binding.speaker == selection.speaker
            && binding.language == selection.language
            && binding.script_sha256 == script_sha256
    })
}

fn selected_revision_scene<'a>(
    manifest: &'a video::VideoProjectManifest,
    requested_scene_id: Option<&str>,
) -> Result<&'a video::ReviewedScene, String> {
    match requested_scene_id {
        Some(scene_id) => manifest
            .reviewed_scenes
            .iter()
            .find(|scene| scene.id == scene_id)
            .ok_or_else(|| {
                "video.scene_not_found: The selected scene is no longer in this version".into()
            }),
        None => manifest.reviewed_scenes.first().ok_or_else(|| {
            "video.scene_required: Plan at least one scene before changing narration".into()
        }),
    }
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn resolve_voice_revision_route(
    runtime: &RuntimeState,
    selection: VoiceRevisionSelection,
) -> Result<ResolvedVoiceRoute, String> {
    let registry = read_json(runtime.model_registry_path.clone(), json!({ "models": [] }));
    let model = registry
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models.iter().find(|model| {
                model.get("model_id").and_then(Value::as_str)
                    == Some(selection.model_id.as_str())
            })
        })
        .ok_or_else(|| {
            format!(
                "video.tts_model_unavailable: The selected local speech model is not installed ({})",
                selection.model_id
            )
        })?;
    if !registry_model_ready_for_task(model, "tts") {
        return Err(
            "video.tts_model_unavailable: The selected speech model is not ready on this machine"
                .into(),
        );
    }
    model
        .get("engine")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("video.invalid_voice_route: The selected model has no engine binding")?;
    if !model_supports_language(model, &selection.language) {
        return Err(format!(
            "video.language_unsupported: {} does not support {}",
            selection.model_id, selection.language
        ));
    }
    let voice = runtime
        .store
        .list_voices()?
        .into_iter()
        .find(|voice| voice.get("id").and_then(Value::as_str) == Some(selection.voice_id.as_str()))
        .ok_or("video.voice_unavailable: The selected soundAr voice was not found")?;
    validate_voice_model_compatibility(&selection, model, &voice)?;
    let voice_name = voice
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(selection.voice_id.as_str())
        .to_string();
    let preset = voice.get("state").and_then(Value::as_str) == Some("preset");
    let reference_audio_path = if preset {
        None
    } else {
        let (_, path) = runtime
            .store
            .voice_reference_for_id(&selection.voice_id)?
            .ok_or("video.voice_consent_required: The selected voice has no active consent-backed reference")?;
        runtime.store.validate_voice_reference(&path)?;
        Some(path)
    };
    Ok(ResolvedVoiceRoute {
        selection,
        voice_name,
        reference_audio_path,
    })
}

fn validate_voice_model_compatibility(
    selection: &VoiceRevisionSelection,
    model: &Value,
    voice: &Value,
) -> Result<(), String> {
    let engine = model
        .get("engine")
        .and_then(Value::as_str)
        .ok_or("video.invalid_voice_route: The selected model has no engine binding")?;
    let state = voice.get("state").and_then(Value::as_str).unwrap_or("");
    let consent = voice.get("consent").and_then(Value::as_str).unwrap_or("");
    let compatible = voice
        .get("engines")
        .and_then(Value::as_array)
        .is_some_and(|engines| {
            engines.iter().any(|candidate| {
                candidate
                    .as_str()
                    .is_some_and(|candidate| engines_compatible(engine, candidate))
            })
        });
    if !compatible {
        return Err("video.voice_model_mismatch: The selected voice is not compatible with this speech model".into());
    }
    match state {
        "preset" if consent == "not-required" => {
            if !matches!(normalize_engine(engine).as_str(), "kokoro")
                || selection.speaker != selection.voice_id
            {
                return Err("video.voice_speaker_mismatch: Preset narration must use the selected preset voice as its engine speaker".into());
            }
        }
        "ready" if consent == "confirmed" => {
            if selection.speaker != "default" {
                return Err("video.voice_speaker_mismatch: Consent-backed reference voices use the engine's default speaker route".into());
            }
        }
        _ => {
            return Err("video.voice_consent_required: The selected voice is not preset or consent-confirmed and ready".into())
        }
    }
    Ok(())
}

fn normalize_engine(value: &str) -> String {
    match value
        .to_ascii_lowercase()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .as_str()
    {
        "foundation" => "kokoro".into(),
        "chatterboxturbo" => "chatterbox".into(),
        "coqui" => "xtts".into(),
        normalized => normalized.to_string(),
    }
}

fn engines_compatible(model_engine: &str, voice_engine: &str) -> bool {
    normalize_engine(model_engine) == normalize_engine(voice_engine)
}

fn model_supports_language(model: &Value, requested: &str) -> bool {
    let requested = requested.to_ascii_lowercase();
    let requested_base = requested.split('-').next().unwrap_or(requested.as_str());
    model
        .get("languages")
        .and_then(Value::as_array)
        .is_some_and(|languages| {
            languages.iter().any(|language| {
                let language = language.as_str().unwrap_or_default().to_ascii_lowercase();
                language == "multilingual"
                    || language == requested
                    || language.split('-').next() == Some(requested_base)
            })
        })
}

fn apply_instruction_revision(
    manifest: &mut video::VideoProjectManifest,
    instruction: &str,
    requested_scene_id: Option<&str>,
    changed_paths: &mut BTreeSet<String>,
    invalidated: &mut BTreeSet<video::RevisionStage>,
) -> Result<(), String> {
    let normalized = instruction.to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(());
    }
    if normalized.contains("shorten the opening")
        || normalized.contains("shorten opening")
        || normalized.contains("trim the opening")
        || normalized.contains("faster opening")
    {
        shorten_opening(manifest)?;
        changed_paths.extend([
            "/reviewed_scenes".into(),
            "/tracks".into(),
            "/gaps".into(),
            "/captions".into(),
            "/timeline_duration_us".into(),
        ]);
        invalidated.extend([
            video::RevisionStage::Plan,
            video::RevisionStage::Captions,
            video::RevisionStage::SceneRender,
            video::RevisionStage::Preview,
            video::RevisionStage::FinalRender,
            video::RevisionStage::PublishPackage,
        ]);
    }
    if normalized.contains("calmer caption") || normalized.contains("captions calmer") {
        set_caption_style(manifest, requested_scene_id, "calm")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("kinetic caption") {
        set_caption_style(manifest, requested_scene_id, "kinetic")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("bold pop caption") || normalized.contains("bold-pop caption") {
        set_caption_style(manifest, requested_scene_id, "bold-pop")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("highlight caption") {
        set_caption_style(manifest, requested_scene_id, "highlight")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("karaoke caption") {
        set_caption_style(manifest, requested_scene_id, "karaoke")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("typewriter caption") {
        set_caption_style(manifest, requested_scene_id, "typewriter")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("podcast caption") {
        set_caption_style(manifest, requested_scene_id, "podcast")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    } else if normalized.contains("clean caption") {
        set_caption_style(manifest, requested_scene_id, "clean-white")?;
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    }
    if normalized.contains("remove captions")
        || normalized.contains("turn off captions")
        || normalized.contains("disable captions")
    {
        if let Some(scene_id) = requested_scene_id {
            set_scene_captions(manifest, scene_id, false, None)?;
        } else {
            manifest.captions.clear();
        }
        changed_paths.insert("/captions".into());
        invalidate_caption_render(invalidated);
    }
    if normalized.contains("portrait layout") || normalized.contains("make it portrait") {
        set_canvas_layout(manifest, "portrait")?;
        changed_paths.insert("/layout".into());
        invalidate_visual_render(invalidated);
    } else if normalized.contains("landscape layout") || normalized.contains("make it landscape") {
        set_canvas_layout(manifest, "landscape")?;
        changed_paths.insert("/layout".into());
        invalidate_visual_render(invalidated);
    } else if normalized.contains("square layout") || normalized.contains("make it square") {
        set_canvas_layout(manifest, "square")?;
        changed_paths.insert("/layout".into());
        invalidate_visual_render(invalidated);
    }
    Ok(())
}

fn set_canvas_layout(
    manifest: &mut video::VideoProjectManifest,
    layout: &str,
) -> Result<(), String> {
    let (mode, width, height) = match layout.trim().to_ascii_lowercase().as_str() {
        "portrait" | "9:16" => (video::CanvasMode::Portrait, 1080, 1920),
        "landscape" | "16:9" => (video::CanvasMode::Landscape, 1920, 1080),
        "square" | "1:1" => (video::CanvasMode::Square, 1080, 1080),
        _ => {
            return Err(
                "video.invalid_layout: Layout must be portrait, landscape, or square".into(),
            )
        }
    };
    manifest.layout.mode = mode;
    manifest.layout.canvas.width = width;
    manifest.layout.canvas.height = height;
    Ok(())
}

fn set_scene_crop(
    manifest: &mut video::VideoProjectManifest,
    scene_id: &str,
    crop_mode: &str,
    manual_crop: Option<video::NormalizedRect>,
) -> Result<(), String> {
    let crop_mode = crop_mode.trim().to_ascii_lowercase();
    if !matches!(crop_mode.as_str(), "auto-center" | "fit" | "manual") {
        return Err("video.invalid_crop: Crop mode must be auto-center, fit, or manual".into());
    }
    let crop = match (crop_mode.as_str(), manual_crop) {
        ("manual", Some(rect))
            if rect.x_bp >= 0
                && rect.y_bp >= 0
                && rect.width_bp > 0
                && rect.height_bp > 0
                && rect.x_bp.checked_add(rect.width_bp).is_some_and(|right| right <= 10_000)
                && rect.y_bp.checked_add(rect.height_bp).is_some_and(|bottom| bottom <= 10_000) => Some(rect),
        ("manual", _) => {
            return Err("video.invalid_crop: Manual framing requires a normalized crop rectangle inside the source frame".into())
        }
        (_, Some(_)) => {
            return Err("video.invalid_crop: Crop rectangles are accepted only in manual mode".into())
        }
        (_, None) => None,
    };
    let mut found = false;
    for track in &mut manifest.tracks {
        if matches!(
            track.kind,
            video::TrackKind::Video | video::TrackKind::Overlay
        ) {
            for clip in &mut track.clips {
                if clip.scene_id.as_deref() == Some(scene_id) {
                    clip.crop = crop;
                    found = true;
                }
            }
        }
    }
    if !found {
        return Err("video.scene_not_renderable: The selected scene has no timeline clip".into());
    }
    manifest.layout.elements.retain(|element| {
        !(element.scene_id.as_deref() == Some(scene_id)
            && matches!(element.role, video::LayoutRole::PrimaryVideo))
    });
    if crop_mode == "auto-center" {
        manifest.layout.elements.push(video::LayoutElement {
            id: format!("primary-{scene_id}"),
            role: video::LayoutRole::PrimaryVideo,
            scene_id: Some(scene_id.into()),
            bounds: video::NormalizedRect {
                x_bp: 0,
                y_bp: 0,
                width_bp: 10_000,
                height_bp: 10_000,
            },
            z_index: 0,
            style_id: Some("auto-center".into()),
        });
    }
    Ok(())
}

fn set_scene_captions(
    manifest: &mut video::VideoProjectManifest,
    scene_id: &str,
    enabled: bool,
    requested_style: Option<&str>,
) -> Result<(), String> {
    let scene = manifest
        .reviewed_scenes
        .iter()
        .find(|scene| scene.id == scene_id)
        .ok_or("video.scene_not_found: The selected scene is no longer in this version")?
        .clone();
    if !enabled {
        manifest
            .captions
            .retain(|cue| cue.scene_id.as_deref() != Some(scene_id));
        return Ok(());
    }
    if manifest
        .captions
        .iter()
        .all(|cue| cue.scene_id.as_deref() != Some(scene_id))
    {
        let end = scene
            .timeline_start_us
            .checked_add(scene.timeline_duration_us)
            .map_err(|error| error.to_string())?;
        manifest.captions.push(video::CaptionCue {
            id: format!("caption-{scene_id}-manual"),
            range: video::TimeRange::new(scene.timeline_start_us.0, end.0)
                .map_err(|error| error.to_string())?,
            text: if scene.script.trim().is_empty() {
                scene.title
            } else {
                scene.script
            },
            style_id: caption_style_id(requested_style.unwrap_or("clean-white"))?,
            speaker_id: None,
            transcript_segment_id: None,
            scene_id: Some(scene_id.into()),
        });
    }
    if let Some(style) = requested_style {
        set_caption_style(manifest, Some(scene_id), style)?;
    }
    Ok(())
}

fn set_caption_style(
    manifest: &mut video::VideoProjectManifest,
    scene_id: Option<&str>,
    style: &str,
) -> Result<(), String> {
    let style_id = caption_style_id(style)?;
    let mut changed = false;
    for caption in &mut manifest.captions {
        if scene_id.is_none() || caption.scene_id.as_deref() == scene_id {
            if caption.style_id != style_id {
                caption.style_id = style_id.clone();
                changed = true;
            }
        }
    }
    if !changed && manifest.captions.is_empty() {
        return Err(
            "video.captions_unavailable: Enable captions before changing their style".into(),
        );
    }
    Ok(())
}

fn caption_style_id(style: &str) -> Result<String, String> {
    video::CaptionPresetId::parse(style)
        .map(|preset| preset.manifest_id().to_string())
        .map_err(|_| {
            format!(
                "video.invalid_caption_style: Caption style must be {}",
                video::CaptionPresetId::PUBLIC_IDS.join(", ")
            )
        })
}

fn set_mix_gain(
    manifest: &mut video::VideoProjectManifest,
    track_id: &str,
    gain_db: f64,
) -> Result<(), String> {
    if !gain_db.is_finite() || !(-96.0..=24.0).contains(&gain_db) {
        return Err("video.invalid_gain: Track gain must be between -96 dB and +24 dB".into());
    }
    let gain_db_milli = (gain_db * 1_000.0).round() as i32;
    if let Some(track) = manifest
        .audio_mix
        .tracks
        .iter_mut()
        .find(|track| track.track_id == track_id)
    {
        track.gain_db_milli = gain_db_milli;
    } else {
        if !manifest.tracks.iter().any(|track| track.id == track_id) {
            if track_id == "music-main" && gain_db_milli == -12_000 {
                return Ok(());
            }
            return Err(format!(
                "video.track_not_found: The project has no {} track to adjust",
                if track_id == "music-main" {
                    "music"
                } else {
                    "audio"
                }
            ));
        }
        manifest.audio_mix.tracks.push(video::AudioMixTrack {
            track_id: track_id.into(),
            gain_db_milli,
            pan_milli: 0,
            ducking: None,
        });
    }
    Ok(())
}

fn shorten_opening(manifest: &mut video::VideoProjectManifest) -> Result<(), String> {
    let first_index = manifest
        .reviewed_scenes
        .iter()
        .enumerate()
        .min_by_key(|(_, scene)| scene.timeline_start_us)
        .map(|(index, _)| index)
        .ok_or("video.scene_required: Plan at least one scene before shortening the opening")?;
    let first = manifest.reviewed_scenes[first_index].clone();
    if first.timeline_duration_us.0 <= 1_250_000 {
        return Err(
            "video.opening_too_short: The opening is already close to the minimum length".into(),
        );
    }
    let reduction = (first.timeline_duration_us.0 / 5).clamp(250_000, 1_500_000);
    let new_duration = video::Microseconds(first.timeline_duration_us.0 - reduction);
    let old_end = first
        .timeline_start_us
        .checked_add(first.timeline_duration_us)
        .map_err(|error| error.to_string())?;
    let new_end = first
        .timeline_start_us
        .checked_add(new_duration)
        .map_err(|error| error.to_string())?;
    let first_id = first.id.clone();
    {
        let opening = &mut manifest.reviewed_scenes[first_index];
        opening.timeline_duration_us = new_duration;
        opening.revision = opening.revision.saturating_add(1);
        if let Some(range) = opening.source_range {
            opening.source_range = Some(
                video::TimeRange::new(range.start_us.0, range.end_us.0 - reduction)
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    for scene in &mut manifest.reviewed_scenes {
        if scene.id != first_id && scene.timeline_start_us >= old_end {
            scene.timeline_start_us = video::Microseconds(scene.timeline_start_us.0 - reduction);
        }
    }
    for clip in manifest
        .tracks
        .iter_mut()
        .flat_map(|track| &mut track.clips)
    {
        if clip.scene_id.as_deref() == Some(first_id.as_str()) {
            clip.timeline_duration_us = new_duration;
            clip.source_range = video::TimeRange::new(
                clip.source_range.start_us.0,
                clip.source_range.end_us.0 - reduction,
            )
            .map_err(|error| error.to_string())?;
        } else if clip.timeline_start_us >= old_end {
            clip.timeline_start_us = video::Microseconds(clip.timeline_start_us.0 - reduction);
        }
    }
    for gap in &mut manifest.gaps {
        if gap.range.start_us >= old_end {
            gap.range = video::TimeRange::new(
                gap.range.start_us.0 - reduction,
                gap.range.end_us.0 - reduction,
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let mut captions = Vec::with_capacity(manifest.captions.len());
    for mut caption in manifest.captions.drain(..) {
        if caption.scene_id.as_deref() == Some(first_id.as_str()) {
            let end = caption.range.end_us.min(new_end);
            if caption.range.start_us < end {
                caption.range = video::TimeRange::new(caption.range.start_us.0, end.0)
                    .map_err(|error| error.to_string())?;
                captions.push(caption);
            }
        } else if caption.range.start_us >= old_end {
            caption.range = video::TimeRange::new(
                caption.range.start_us.0 - reduction,
                caption.range.end_us.0 - reduction,
            )
            .map_err(|error| error.to_string())?;
            captions.push(caption);
        } else {
            captions.push(caption);
        }
    }
    manifest.captions = captions;
    manifest.timeline_duration_us = video::Microseconds(
        manifest
            .timeline_duration_us
            .0
            .checked_sub(reduction)
            .ok_or("video.invalid_timeline: Opening revision underflowed the timeline")?,
    );
    Ok(())
}

fn manifest_diff_paths(
    before: &video::VideoProjectManifest,
    after: &video::VideoProjectManifest,
) -> Result<BTreeSet<String>, String> {
    video::manifest_changed_paths(before, after).map_err(service_error)
}

fn invalidation_for_manifest_changes(paths: &BTreeSet<String>) -> BTreeSet<video::RevisionStage> {
    video::invalidated_stages_for_manifest_changes(paths)
}

fn invalidate_caption_render(stages: &mut BTreeSet<video::RevisionStage>) {
    stages.extend([
        video::RevisionStage::Captions,
        video::RevisionStage::SceneRender,
        video::RevisionStage::Preview,
        video::RevisionStage::FinalRender,
        video::RevisionStage::PublishPackage,
    ]);
}

fn invalidate_visual_render(stages: &mut BTreeSet<video::RevisionStage>) {
    stages.extend([
        video::RevisionStage::SceneRender,
        video::RevisionStage::Preview,
        video::RevisionStage::FinalRender,
        video::RevisionStage::PublishPackage,
    ]);
}

fn invalidate_audio_render(stages: &mut BTreeSet<video::RevisionStage>) {
    stages.extend([
        video::RevisionStage::SceneRender,
        video::RevisionStage::Preview,
        video::RevisionStage::FinalRender,
        video::RevisionStage::PublishPackage,
    ]);
}

fn discard_invalidated_render_artifacts(
    manifest: &mut video::VideoProjectManifest,
    stages: &BTreeSet<video::RevisionStage>,
) {
    manifest
        .render_artifacts
        .retain(|artifact| !match artifact.role {
            video::RenderArtifactRole::Captions => stages.contains(&video::RevisionStage::Captions),
            video::RenderArtifactRole::SceneSegment => {
                stages.contains(&video::RevisionStage::SceneRender)
            }
            video::RenderArtifactRole::Preview => stages.contains(&video::RevisionStage::Preview),
            video::RenderArtifactRole::FinalMaster => {
                stages.contains(&video::RevisionStage::FinalRender)
            }
            video::RenderArtifactRole::PublishPackage => {
                stages.contains(&video::RevisionStage::PublishPackage)
            }
            _ => false,
        });
}

fn analyze_project(
    runtime: &RuntimeState,
    project_id: &str,
    language: Option<String>,
    progress: Option<&video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    let record = runtime
        .video
        .get_project(project_id)
        .map_err(service_error)?;
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    let source = manifest
        .source_assets
        .iter()
        .find(|source| source.probe.has_audio)
        .cloned()
        .ok_or("video.audio_required: The source contains no audio to analyze")?;
    let model_id = installed_whisper_model(runtime).ok_or(
        "video.transcriber_unavailable: Install a local Whisper speech-to-text model in Models",
    )?;
    let request = DurableAnalyzeRequest {
        project_id: project_id.to_string(),
        source_asset_id: source.id,
        source_sha256: source.sha256,
        model_id,
        language,
        expected_revision: record
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or("video.invalid_project: Project revision is missing")?,
        expected_version_id: project_version_id(&record)?.to_string(),
        priority: "normal".into(),
        title: format!("Analyze {}", manifest.name),
    };
    let request_value = serde_json::to_value(&request)
        .map_err(|error| format!("video.invalid_request: {error}"))?;
    let job_id = runtime.store.create_job("video_analyze", &request_value)?;
    let project = run_analyze_job_guarded(runtime, &job_id, request, progress)?;
    Ok(ProjectOperationResult {
        project,
        job_id: Some(job_id),
    })
}

fn run_analyze_job_guarded(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurableAnalyzeRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_analyze_job(runtime, job_id, request, progress)
    }))
    .unwrap_or_else(|_| {
        Err("video.worker_panicked: The analysis worker stopped unexpectedly".into())
    });
    if let Err(error) = &result {
        persist_command_job_failure(runtime, job_id, error);
    }
    result
}

fn run_analyze_job(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurableAnalyzeRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let record = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;
    ensure_durable_project_expectation(
        &record,
        request.expected_revision,
        &request.expected_version_id,
    )?;
    let mut manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    let source = manifest
        .source_assets
        .iter()
        .find(|source| source.id == request.source_asset_id)
        .filter(|source| source.probe.has_audio)
        .cloned()
        .ok_or(
            "video.source_changed: The analyzed audio source no longer belongs to this version",
        )?;
    if source.sha256 != request.source_sha256 {
        return Err(
            "video.source_changed: The analyzed audio source no longer matches the durable task"
                .into(),
        );
    }
    if !registered_model(runtime, &request.model_id) {
        return Err(format!(
            "video.transcriber_unavailable: The selected local Whisper model is no longer installed ({})",
            request.model_id
        ));
    }
    runtime.store.update_job(&job_id, "preparing", 0.05)?;
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "analyze",
        0.05,
        "Preparing source audio",
        None,
    );
    let source_path = runtime
        .store
        .video_artifacts_root()
        .join(&source.managed_path);
    let analysis_dir = runtime
        .store
        .video_artifacts_root()
        .join("projects")
        .join(&request.project_id)
        .join("analysis");
    fs::create_dir_all(&analysis_dir)
        .map_err(|error| format!("video.storage_unavailable: {error}"))?;
    let audio_path = analysis_dir.join(format!("audio-{}.wav", &source.sha256[..16]));
    if !audio_path.is_file() {
        extract_analysis_audio(
            runtime,
            job_id,
            &request.project_id,
            &source_path,
            &audio_path,
            progress,
        )?;
    }
    ensure_command_job_active(runtime, job_id)?;
    runtime.store.update_job(job_id, "preparing", 0.34)?;
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "analyze",
        0.34,
        "Transcribing on the original source clock",
        None,
    );
    let evidence = runtime.request_for_job(
        json!({
            "operation": "transcribe",
            "model_id": request.model_id,
            "audio_path": audio_path,
            "language": request.language,
            "task": "transcribe",
            "priority": request.priority,
        }),
        job_id,
    )?;
    ensure_command_job_active(runtime, job_id)?;
    let transcript = video::transcript_from_runtime_json(
        &evidence,
        &video::TranscriptImportRequest {
            source_asset_id: source.id.clone(),
            source_clock_duration_us: source.probe.duration_us,
            timing_source: video::TranscriptTimingSource::SoundArWhisper,
            created_at: utc_now(),
        },
    )
    .map_err(|error| error.to_string())?;
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "analyze",
        0.76,
        "Identifying complete candidate moments",
        None,
    );
    let policy = candidate_policy(source.probe.duration_us);
    let analysis = video::identify_clip_candidates(&transcript, &policy, &BTreeSet::new())
        .map_err(|error| error.to_string())?;
    if analysis.candidates.is_empty() {
        return Err(
            "video.no_candidates: The transcript did not contain a complete clip candidate".into(),
        );
    }
    ensure_command_job_active(runtime, job_id)?;
    manifest.transcript = Some(transcript);
    manifest.candidates = analysis.candidates;
    let changed_paths = BTreeSet::from(["/transcript".to_string(), "/candidates".to_string()]);
    let invalidated_stages = invalidation_for_manifest_changes(&changed_paths);
    let changed_paths = changed_paths.into_iter().collect::<Vec<_>>();
    advance_manifest_revision(
        &mut manifest,
        "soundar-video-analyzer",
        "Analyze source transcript and candidate clips",
        changed_paths.clone(),
        invalidated_stages.clone(),
    )?;
    let revised = runtime
        .video
        .revise_manifest(video::ReviseVideoManifestRequest {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision,
            manifest,
            actor: "soundar-video-analyzer".into(),
            reason: "Analyze source transcript and candidate clips".into(),
            changed_paths,
            invalidated_stages,
            status: Some("review".into()),
        })
        .map_err(service_error)?;
    ensure_command_job_active(runtime, job_id)?;
    runtime.store.complete_job(job_id)?;
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "analyze",
        1.0,
        "Analysis ready for review",
        None,
    );
    video::present_video_project(&revised, &runtime.store.video_artifacts_root())
        .map_err(|error| error.to_string())
}

fn plan_project(
    runtime: &RuntimeState,
    project_id: &str,
    selected_candidate_ids: Option<Vec<String>>,
    creative_brief: Option<String>,
    progress: Option<&video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    let record = runtime
        .video
        .get_project(project_id)
        .map_err(service_error)?;
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    let selected = selected_candidate_ids.unwrap_or_else(|| {
        manifest
            .candidates
            .iter()
            .take(3)
            .map(|candidate| candidate.id.clone())
            .collect()
    });
    let request = DurablePlanRequest {
        project_id: project_id.to_string(),
        selected_candidate_ids: selected,
        creative_brief,
        expected_revision: record
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or("video.invalid_project: Project revision is missing")?,
        expected_version_id: project_version_id(&record)?.to_string(),
        priority: "normal".into(),
        title: format!("Plan {}", manifest.name),
    };
    let request_value = serde_json::to_value(&request)
        .map_err(|error| format!("video.invalid_request: {error}"))?;
    let job_id = runtime.store.create_job("video_plan", &request_value)?;
    let project = run_plan_job_guarded(runtime, &job_id, request, progress)?;
    Ok(ProjectOperationResult {
        project,
        job_id: Some(job_id),
    })
}

fn run_plan_job_guarded(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurablePlanRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_plan_job(runtime, job_id, request, progress)
    }))
    .unwrap_or_else(|_| {
        Err("video.worker_panicked: The scene-planning worker stopped unexpectedly".into())
    });
    if let Err(error) = &result {
        persist_command_job_failure(runtime, job_id, error);
    }
    result
}

fn run_plan_job(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurablePlanRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let record = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;
    ensure_durable_project_expectation(
        &record,
        request.expected_revision,
        &request.expected_version_id,
    )?;
    let mut manifest: video::VideoProjectManifest = serde_json::from_value(
        record
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    let status = runtime.store.start_job(job_id)?;
    if status == "cancelled" {
        return Err("video.cancelled: Scene planning was cancelled".into());
    }
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "review",
        0.3,
        "Building the reviewed scene timeline",
        None,
    );
    ensure_command_job_active(runtime, job_id)?;
    let original_manifest = manifest.clone();
    let plan = video::plan_reviewed_timeline(
        &manifest,
        &video::ScenePlanRequest {
            selected_candidate_ids: request.selected_candidate_ids,
            caption_style_id: "caption-clean-white".into(),
            inter_scene_gap_us: video::Microseconds::ZERO,
        },
    )
    .map_err(|error| error.to_string())?;
    video::apply_scene_plan(&mut manifest, plan).map_err(|error| error.to_string())?;
    let changed_path_set = manifest_diff_paths(&original_manifest, &manifest)?;
    if changed_path_set.is_empty() {
        return Err(
            "video.plan_unchanged: The selected candidates already match this timeline".into(),
        );
    }
    let stages = invalidation_for_manifest_changes(&changed_path_set);
    let changed_paths = changed_path_set.into_iter().collect::<Vec<_>>();
    let reason = request
        .creative_brief
        .as_deref()
        .map(str::trim)
        .filter(|brief| !brief.is_empty())
        .map(|brief| {
            let brief = brief.chars().take(1_000).collect::<String>();
            format!("Approve candidates with Codex creative brief: {brief}")
        })
        .unwrap_or_else(|| "Approve candidates and build the scene timeline".into());
    ensure_command_job_active(runtime, job_id)?;
    advance_manifest_revision(
        &mut manifest,
        "local-user",
        &reason,
        changed_paths.clone(),
        stages.clone(),
    )?;
    let revised = runtime
        .video
        .revise_manifest(video::ReviseVideoManifestRequest {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision,
            manifest,
            actor: "local-user".into(),
            reason,
            changed_paths,
            invalidated_stages: stages,
            status: Some("ready".into()),
        })
        .map_err(service_error)?;
    ensure_command_job_active(runtime, job_id)?;
    runtime.store.complete_job(job_id)?;
    emit_operation_progress(
        progress,
        job_id,
        &request.project_id,
        "review",
        1.0,
        "Scene timeline is ready",
        None,
    );
    video::present_video_project(&revised, &runtime.store.video_artifacts_root())
        .map_err(|error| error.to_string())
}

fn ensure_durable_project_expectation(
    record: &Value,
    expected_revision: i64,
    expected_version_id: &str,
) -> Result<(), String> {
    let current_revision = record
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Project revision is missing")?;
    let current_version_id = project_version_id(record)?;
    if current_revision != expected_revision || current_version_id != expected_version_id {
        return Err(format!(
            "video.resume_conflict: The durable task targets revision {expected_revision} ({expected_version_id}), but the project is now revision {current_revision} ({current_version_id})"
        ));
    }
    Ok(())
}

fn ensure_command_job_active(runtime: &RuntimeState, job_id: &str) -> Result<(), String> {
    match runtime.store.job_status(job_id)?.as_deref() {
        Some("queued" | "preparing" | "running") => Ok(()),
        Some("cancelled") => Err("video.cancelled: The Video Studio task was cancelled".into()),
        Some(status) => Err(format!(
            "video.job_not_active: The Video Studio task cannot continue from {status}"
        )),
        None => Err("video.job_not_found: The Video Studio task was not found".into()),
    }
}

fn persist_command_job_failure(runtime: &RuntimeState, job_id: &str, error: &str) {
    if runtime.store.job_status(job_id).ok().flatten().as_deref() != Some("cancelled") {
        runtime.store.fail_job(job_id, error).ok();
    }
}

fn registered_model(runtime: &RuntimeState, model_id: &str) -> bool {
    read_json(runtime.model_registry_path.clone(), json!({ "models": [] }))
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models.iter().any(|model| {
                model.get("model_id").and_then(Value::as_str) == Some(model_id)
                    && registry_model_ready_for_task(model, "stt")
            })
        })
}

fn extract_analysis_audio(
    runtime: &RuntimeState,
    job_id: &str,
    project_id: &str,
    source: &Path,
    output: &Path,
    progress_callback: Option<&video::ProgressCallback>,
) -> Result<(), String> {
    let status = runtime.video.runtime_status(false);
    let ffmpeg = status
        .ffmpeg
        .path
        .as_deref()
        .filter(|_| status.ffmpeg.available)
        .ok_or("video.ffmpeg_unavailable: FFmpeg is required to extract analysis audio")?;
    let staging = output.with_file_name(format!(
        ".{}.{}.partial.wav",
        output
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("analysis"),
        Uuid::new_v4().simple()
    ));
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-n"]);
    command.args(video::local_media_input_args(source).map_err(|error| error.to_string())?);
    let mut child = command
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("video.audio_extract_failed: {error}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("video.audio_extract_failed: {error}"))?
        {
            break status;
        }
        if runtime.store.job_status(job_id)?.as_deref() == Some("cancelled") {
            video::terminate_process_group(&mut child, Duration::from_secs(2))
                .map_err(|error| error.to_string())?;
            fs::remove_file(&staging).ok();
            return Err("video.cancelled: Analysis was cancelled".into());
        }
        let progress = (0.08 + started.elapsed().as_secs_f64().min(8.0) / 80.0).min(0.22);
        runtime.store.update_job(job_id, "preparing", progress)?;
        emit_operation_progress(
            progress_callback,
            job_id,
            project_id,
            "analyze",
            progress,
            "Preparing source audio",
            None,
        );
        thread::sleep(Duration::from_millis(100));
    };
    if !status.success() || fs::metadata(&staging).map(|value| value.len()).unwrap_or(0) <= 44 {
        fs::remove_file(&staging).ok();
        return Err("video.audio_extract_failed: FFmpeg could not prepare source audio".into());
    }
    fs::rename(&staging, output).map_err(|error| format!("video.audio_extract_failed: {error}"))?;
    Ok(())
}

fn create_empty_project(
    service: &video::VideoStudioService,
    name: &str,
    duration_us: i64,
    intent: Option<String>,
    actor: &str,
) -> Result<Value, String> {
    let project_id = Uuid::new_v4().simple().to_string();
    create_empty_project_with_id(service, &project_id, name, duration_us, intent, actor)
}

fn create_empty_project_with_id(
    service: &video::VideoStudioService,
    project_id: &str,
    name: &str,
    duration_us: i64,
    intent: Option<String>,
    actor: &str,
) -> Result<Value, String> {
    let name = bounded_name(name, "Untitled video");
    let manifest = empty_project_manifest(project_id, &name, duration_us)?;
    service
        .create_project(video::ServiceCreateVideoProjectRequest {
            name,
            manifest,
            actor: actor.into(),
            initial_intent: intent,
        })
        .map_err(service_error)
}

fn empty_project_manifest(
    project_id: &str,
    name: &str,
    duration_us: i64,
) -> Result<video::VideoProjectManifest, String> {
    video::VideoProjectManifest::new(
        project_id,
        name,
        video::RationalFrameRate::FPS_30,
        video::Microseconds(duration_us.max(1_000_000)),
        video::LayoutPlan {
            mode: video::CanvasMode::Portrait,
            canvas: video::CanvasSpec {
                width: 1080,
                height: 1920,
                pixel_aspect_numerator: 1,
                pixel_aspect_denominator: 1,
            },
            safe_area: video::NormalizedRect {
                x_bp: 600,
                y_bp: 500,
                width_bp: 8_800,
                height_bp: 9_000,
            },
            background_rgba: [24, 24, 24, 255],
            elements: vec![],
        },
        video::AudioMix {
            target_lufs_milli: -14_000,
            true_peak_db_milli: -1_000,
            tracks: vec![],
        },
        utc_now(),
    )
    .map_err(|error| error.to_string())
}

fn preview_link_value(
    service: &video::VideoStudioService,
    exact_url: &str,
) -> Result<Value, String> {
    let preview = service.preview_link(exact_url).map_err(service_error)?;
    Ok(json!({
        "exact_url": preview.canonical_url,
        "title": preview.title,
        "creator": preview.creator.unwrap_or_else(|| "Unknown creator".into()),
        "duration_ms": preview.duration_us.unwrap_or_default() / 1_000,
        "published_label": "Source metadata",
        "preview_url": Value::Null,
        "poster_url": preview.thumbnail_url,
        "is_single_source": true,
    }))
}

fn import_link_project(
    runtime: &RuntimeState,
    request: UiImportLinkRequest,
    actor: &str,
    progress: Option<video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    if !request.rights_confirmed || !request.single_source_only {
        return Err(
            "video.rights_required: Confirm rights for exactly one source before importing".into(),
        );
    }
    let validated =
        video::validate_import_url(&request.exact_url).map_err(|error| error.to_string())?;
    if validated.is_playlist {
        return Err("video.playlist_not_allowed: Import one exact source URL at a time".into());
    }
    if request.rights_confirmation_url != validated.canonical {
        return Err(
            "video.rights_url_mismatch: Rights confirmation must match the exact canonical URL"
                .into(),
        );
    }
    let project = create_empty_project(
        &runtime.video,
        "Imported source · Reel draft",
        1_000_000,
        Some(format!("Import authorized source {}", validated.canonical)),
        actor,
    )?;
    let project_id = value_string(&project, "id")?;
    let queued = runtime
        .video
        .queue_link_import(
            video::LinkImportRequest {
                project_id: project_id.clone(),
                url: validated.canonical.clone(),
                actor: actor.into(),
                rights: video::LinkRightsRequest {
                    confirmed_url: validated.canonical,
                    basis: video::RightsBasis::Other,
                    statement: "I confirm that I own this media or have permission to use this exact URL in this project.".into(),
                    confirmed_by: actor.into(),
                },
                title: Some("Imported source".into()),
            },
            progress,
        )
        .map_err(service_error)?;
    let result = runtime
        .video
        .wait_for_job(&queued.job_id, &project_id, VIDEO_COMMAND_TIMEOUT)
        .map_err(service_error)?;
    let project =
        video::present_video_project(&result.project, &runtime.store.video_artifacts_root())
            .map_err(|error| error.to_string())?;
    Ok(ProjectOperationResult {
        project,
        job_id: Some(queued.job_id),
    })
}

fn create_project_from_request(
    runtime: &RuntimeState,
    mut request: UiCreateProjectRequest,
    actor: &str,
    audio_authority: AudioPathAuthority,
    progress: Option<video::ProgressCallback>,
) -> Result<ProjectOperationResult, String> {
    let prompt = request.prompt.trim().to_string();
    if prompt.chars().count() > 12_000 {
        return Err("video.invalid_prompt: Video prompts are limited to 12,000 characters".into());
    }
    if prompt.is_empty()
        && request.audio_local_path.is_none()
        && request.source_project_id.is_none()
    {
        return Err(
            "video.intent_required: Describe the video, choose audio, or select a soundAr project"
                .into(),
        );
    }
    if request.source_project_id.is_some() && request.audio_local_path.is_some() {
        return Err(
            "video.single_source_required: Start with one soundAr project or one audio source"
                .into(),
        );
    }

    if let Some(source_project_id) = request.source_project_id.take() {
        let source_project = runtime
            .store
            .list_projects()?
            .into_iter()
            .find(|project| project.get("id").and_then(Value::as_str) == Some(&source_project_id))
            .ok_or("video.source_project_not_found: The selected soundAr project was not found")?;
        let history_id = source_project
            .pointer("/document/master/history_id")
            .and_then(Value::as_str)
            .ok_or("video.source_project_unrendered: Export a playable master from the soundAr project before using it in Video Studio")?;
        let history = runtime
            .store
            .get_history(history_id)?
            .ok_or("video.source_project_unavailable: The soundAr project master is no longer available in History")?;
        request.audio_local_path = history
            .get("audio_path")
            .and_then(Value::as_str)
            .map(str::to_string);
        request.audio_display_name = source_project
            .get("name")
            .and_then(Value::as_str)
            .map(|name| format!("{name} master"));
    }

    if audio_authority == AudioPathAuthority::ManagedAgentReference {
        if let Some(path) = request.audio_local_path.as_deref() {
            authorize_agent_audio_path(runtime, path)?;
        }
    }

    let name = prompt_title(&prompt);
    let prompt_route = if request.audio_local_path.is_none() {
        Some(installed_tts_route(runtime).ok_or(
            "video.tts_unavailable: Install a supported local speech model or choose existing audio",
        )?)
    } else {
        None
    };
    if let Some((model_id, speaker, voice_name)) = prompt_route {
        // Reserve the canonical project id and persist the owning workflow before any project or
        // speech mutation. A crash at the next instruction leaves a visible durable job whose
        // runner can idempotently create this exact draft on resume.
        let project_id = Uuid::new_v4().simple().to_string();
        let durable = DurablePromptVideoRequest {
            project_id: project_id.clone(),
            prompt_sha256: sha256_text(&prompt),
            prompt,
            title: name,
            actor: actor.to_string(),
            model_id,
            speaker,
            voice_name,
            language: "en".into(),
            priority: "normal".into(),
        };
        let durable_value = serde_json::to_value(&durable)
            .map_err(|error| format!("video.invalid_request: {error}"))?;
        let mut initial_manifest = empty_project_manifest(&project_id, &durable.title, 5_000_000)?;
        initial_manifest.revision = 1;
        initial_manifest.updated_at = utc_now();
        initial_manifest
            .revision_history
            .push(video::RevisionRecord {
                id: Uuid::new_v4().simple().to_string(),
                revision: 1,
                parent_id: None,
                actor: actor.to_string(),
                reason: format!(
                    "Initial intent: {}",
                    durable.prompt.chars().take(3_800).collect::<String>()
                ),
                changed_paths: vec!["/".into()],
                invalidated_stages: BTreeSet::new(),
                created_at: initial_manifest.updated_at.clone(),
            });
        initial_manifest
            .validate_strict()
            .map_err(|error| error.to_string())?;
        let initial_manifest_value = serde_json::to_value(&initial_manifest)
            .map_err(|error| format!("video.invalid_manifest: {error}"))?;
        let (_project, parent_job_id) = runtime.store.create_video_project_with_job(
            &durable.title,
            &initial_manifest_value,
            actor,
            "video_create_from_prompt",
            &durable_value,
            &format!("video-create-prompt:{project_id}"),
        )?;
        let record =
            run_prompt_video_job_guarded(runtime, &parent_job_id, durable, progress.as_ref())?;
        let project = video::present_video_project(&record, &runtime.store.video_artifacts_root())
            .map_err(|error| error.to_string())?;
        return Ok(ProjectOperationResult {
            project,
            job_id: Some(parent_job_id),
        });
    }

    let project = create_empty_project(
        &runtime.video,
        &name,
        5_000_000,
        (!prompt.is_empty()).then_some(prompt.clone()),
        actor,
    )?;
    let project_id = value_string(&project, "id")?;

    let source_path = request
        .audio_local_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .ok_or(
            "video.audio_unavailable: The generated or selected audio has no playable artifact",
        )?;
    let queued = runtime
        .video
        .queue_local_import(
            video::LocalImportRequest {
                project_id: project_id.clone(),
                source_path,
                actor: actor.into(),
                title: request.audio_display_name,
            },
            progress,
        )
        .map_err(service_error)?;
    let result = runtime
        .video
        .wait_for_job(&queued.job_id, &project_id, VIDEO_COMMAND_TIMEOUT)
        .map_err(service_error)?;
    let project =
        video::present_video_project(&result.project, &runtime.store.video_artifacts_root())
            .map_err(|error| error.to_string())?;
    Ok(ProjectOperationResult {
        project,
        job_id: Some(queued.job_id),
    })
}

fn authorize_agent_audio_path(runtime: &RuntimeState, raw_path: &str) -> Result<(), String> {
    if runtime
        .store
        .get_registered_history_by_audio_path(raw_path)?
        .is_some()
    {
        return Ok(());
    }
    if runtime
        .store
        .get_registered_video_audio_by_path(raw_path)?
        .is_some()
    {
        return Ok(());
    }
    Err("video.agent_audio_unauthorized: The assistant may use only integrity-verified soundAr History audio or registered Video Studio audio".into())
}

fn authorize_agent_export_destination(raw_path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(raw_path);
    if !requested.is_absolute() {
        return Err(
            "video.export_destination_invalid: Choose an explicit absolute export directory".into(),
        );
    }
    let metadata = fs::symlink_metadata(&requested).map_err(|error| {
        format!("video.export_destination_invalid: The export directory is unavailable: {error}")
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(
            "video.export_destination_invalid: The export destination must be a regular directory, not a symlink"
                .into(),
        );
    }
    let canonical = requested.canonicalize().map_err(|error| {
        format!(
            "video.export_destination_invalid: The export directory could not be opened: {error}"
        )
    })?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|path| path.canonicalize().ok());
    if canonical == Path::new("/") || home.as_ref().is_some_and(|home| &canonical == home) {
        return Err(
            "video.export_destination_too_broad: Choose a dedicated folder instead of the filesystem or home-directory root"
                .into(),
        );
    }
    Ok(canonical)
}

fn run_prompt_video_job_guarded(
    runtime: &RuntimeState,
    job_id: &str,
    request: DurablePromptVideoRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        run_prompt_video_job(runtime, job_id, &request, progress)
    }))
    .unwrap_or_else(|_| {
        Err("video.worker_panicked: The prompt-to-video worker stopped unexpectedly".into())
    });
    if let Err(error) = &result {
        persist_command_job_failure(runtime, job_id, error);
    }
    result
}

fn run_prompt_video_job(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurablePromptVideoRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Value, String> {
    let status = runtime.store.start_job(parent_job_id)?;
    if status == "cancelled" {
        return Err("video.cancelled: Prompt-to-video creation was cancelled".into());
    }
    ensure_command_job_active(runtime, parent_job_id)?;
    if request.prompt.trim().is_empty()
        || sha256_text(request.prompt.trim()) != request.prompt_sha256
    {
        return Err(
            "video.resume_invalid: The durable prompt identity no longer matches its script".into(),
        );
    }
    if let Some(project) = adopt_or_resume_prompt_import(runtime, parent_job_id, request, progress)?
    {
        return Ok(project);
    }

    let _initial = ensure_prompt_project(runtime, request)?;
    runtime.store.update_job(parent_job_id, "running", 0.08)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "source",
        0.08,
        "Preparing the installed local narration voice",
        None,
    );

    let synthesis_request = prompt_synthesis_request(request, parent_job_id);
    let (history, synthesis_job_id) = run_prompt_synthesis_child(
        runtime,
        parent_job_id,
        request,
        &synthesis_request,
        progress,
    )?;
    validate_prompt_history(runtime, request, &synthesis_request, &history)?;
    let history_id = value_string(&history, "id")?;
    let audio_path = history
        .get("audio_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or("video.tts_failed: Prompt narration has no registered audio artifact")?;
    runtime.store.update_job(parent_job_id, "running", 0.58)?;
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "source",
        0.58,
        "Narration is ready; registering it on the Video Studio timeline",
        Some(history.clone()),
    );
    ensure_command_job_active(runtime, parent_job_id)?;

    let queued = runtime
        .video
        .queue_local_import_idempotent(
            prompt_local_import_request(request, audio_path),
            parent_job_id,
            None,
        )
        .map_err(service_error)?;
    if let Err(error) = ensure_command_job_active(runtime, parent_job_id) {
        runtime.video.cancel_job(&queued.job_id).ok();
        return Err(error);
    }
    runtime.store.update_job(parent_job_id, "running", 0.72)?;
    let completed = runtime
        .video
        .wait_for_job(&queued.job_id, &request.project_id, VIDEO_COMMAND_TIMEOUT)
        .map_err(service_error)?;
    ensure_command_job_active(runtime, parent_job_id)?;
    validate_prompt_imported_project(&completed.project, request, &history_id, &synthesis_job_id)?;
    if !runtime.store.complete_job(parent_job_id)? {
        return Err(
            "video.parent_job_inactive: Prompt-to-video creation did not remain active through completion"
                .into(),
        );
    }
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "source",
        1.0,
        "Prompt narration is registered and ready for scene planning",
        None,
    );
    Ok(completed.project)
}

fn ensure_prompt_project(
    runtime: &RuntimeState,
    request: &DurablePromptVideoRequest,
) -> Result<Value, String> {
    let project = match runtime.store.get_video_project(&request.project_id)? {
        Some(project) => project,
        None => {
            return Err(
                "video.integrity_failed: The atomic prompt parent exists without its project"
                    .into(),
            )
        }
    };
    let revision = project
        .get("revision")
        .and_then(Value::as_i64)
        .ok_or("video.invalid_project: Prompt project revision is missing")?;
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        project
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Prompt project manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    if revision != 1
        || manifest.revision != 1
        || manifest.project_id != request.project_id
        || manifest.name != request.title
        || !manifest.source_assets.is_empty()
        || !manifest.tracks.is_empty()
        || manifest.revision_history.len() != 1
    {
        return Err(
            "video.resume_conflict: The reserved prompt project is no longer its pristine durable draft"
                .into(),
        );
    }
    let expected_reason = format!(
        "Initial intent: {}",
        request.prompt.chars().take(3_800).collect::<String>()
    );
    if manifest.revision_history[0].actor != request.actor
        || manifest.revision_history[0].reason != expected_reason
    {
        return Err(
            "video.resume_conflict: The prompt project initial intent does not match its durable parent"
                .into(),
        );
    }
    Ok(project)
}

fn prompt_synthesis_request(request: &DurablePromptVideoRequest, parent_job_id: &str) -> Value {
    let seed = u32::from_str_radix(&request.prompt_sha256[..8], 16).unwrap_or(42_817);
    json!({
        "operation": "synthesize",
        "generation_kind": "speech",
        "model_id": request.model_id,
        "text": request.prompt,
        "input_mode": "text",
        "speaker": request.speaker,
        "language": request.language,
        "speed": 1.0,
        "seed": seed,
        "output_format": "wav",
        "title": format!("{} narration", request.title),
        "voice_name": request.voice_name,
        "priority": request.priority,
        "video_parent_job_id": parent_job_id,
        "video_project_id": request.project_id,
        "video_purpose": "prompt_narration",
        "video_source_role": "primary_narration",
    })
}

fn run_prompt_synthesis_child(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurablePromptVideoRequest,
    synthesis_request: &Value,
    progress: Option<&video::ProgressCallback>,
) -> Result<(Value, String), String> {
    let Some((child_job_id, created)) = runtime.store.create_idempotent_job(
        "synthesis",
        &format!("video-prompt-synthesis-{parent_job_id}"),
        synthesis_request,
    )?
    else {
        return Err(
            "video.resume_conflict: The prompt parent is already bound to different synthesis inputs"
                .into(),
        );
    };
    ensure_command_job_active(runtime, parent_job_id).map_err(|error| {
        runtime.cancel_job(&child_job_id).ok();
        error
    })?;

    let run_child = |child_request: Value| -> Result<Value, String> {
        emit_operation_progress(
            progress,
            parent_job_id,
            &request.project_id,
            "source",
            0.24,
            "Generating narration with the installed local voice",
            None,
        );
        let outcome = runtime
            .request_for_job(child_request.clone(), &child_job_id)
            .and_then(|result| {
                runtime
                    .store
                    .complete_synthesis(&child_job_id, &child_request, &result)
            });
        if let Err(error) = &outcome {
            if runtime.store.job_status(&child_job_id)?.as_deref() != Some("cancelled") {
                runtime.store.fail_job(&child_job_id, error)?;
            }
        }
        outcome.map_err(|error| format!("video.tts_failed: {error}"))
    };

    let history = if created {
        run_child(synthesis_request.clone())?
    } else {
        match runtime.store.job_status(&child_job_id)?.as_deref() {
            Some("completed") => runtime.store.get_history_by_job_id(&child_job_id)?.ok_or(
                "video.tts_failed: The completed prompt narration has no History artifact",
            )?,
            Some("failed" | "cancelled") => {
                let (_, stored_request) = runtime.store.retry_synthesis_job(&child_job_id)?;
                if stored_request != *synthesis_request {
                    return Err(
                        "video.resume_conflict: The stored prompt synthesis inputs changed".into(),
                    );
                }
                run_child(stored_request)?
            }
            Some("queued" | "preparing") => run_child(synthesis_request.clone())?,
            Some("running") => wait_for_narration_history(
                runtime,
                parent_job_id,
                &child_job_id,
                VIDEO_COMMAND_TIMEOUT,
            )?,
            Some(state) => {
                return Err(format!(
                    "video.tts_failed: The prompt narration child cannot continue from {state}"
                ))
            }
            None => return Err("video.tts_failed: The prompt narration child disappeared".into()),
        }
    };
    Ok((history, child_job_id))
}

fn validate_prompt_history(
    runtime: &RuntimeState,
    request: &DurablePromptVideoRequest,
    synthesis_request: &Value,
    history: &Value,
) -> Result<(), String> {
    if history.get("generation_kind").and_then(Value::as_str) != Some("speech")
        || history.get("model_id").and_then(Value::as_str) != Some(request.model_id.as_str())
        || !narration_history_artifact_is_intact(history)
    {
        return Err(
            "video.prompt_history_mismatch: Prompt narration is not intact registered speech"
                .into(),
        );
    }
    let history_id = value_string(history, "id")?;
    let stored_request = runtime.store.history_request(&history_id)?;
    for field in [
        "model_id",
        "text",
        "speaker",
        "language",
        "video_parent_job_id",
        "video_project_id",
        "video_purpose",
        "video_source_role",
    ] {
        if stored_request.get(field) != synthesis_request.get(field) {
            return Err(format!(
                "video.prompt_history_mismatch: Prompt narration provenance field {field} changed"
            ));
        }
    }
    let audio_path = history
        .get("audio_path")
        .and_then(Value::as_str)
        .ok_or("video.prompt_history_mismatch: Prompt narration has no audio path")?;
    let registered = runtime
        .store
        .get_registered_history_by_audio_path(audio_path)?
        .ok_or("video.prompt_history_mismatch: Prompt narration is not registered in History")?;
    if registered.get("id") != history.get("id") {
        return Err(
            "video.prompt_history_mismatch: Prompt narration path resolves to another History item"
                .into(),
        );
    }
    Ok(())
}

fn adopt_or_resume_prompt_import(
    runtime: &RuntimeState,
    parent_job_id: &str,
    request: &DurablePromptVideoRequest,
    progress: Option<&video::ProgressCallback>,
) -> Result<Option<Value>, String> {
    let Some((import_job_id, mut import_status)) = runtime
        .store
        .video_child_job(parent_job_id, "video_import_local")?
    else {
        return Ok(None);
    };
    let Some((synthesis_job_id, synthesis_status)) =
        runtime.store.video_child_job(parent_job_id, "synthesis")?
    else {
        return Err(
            "video.prompt_history_mismatch: Completed prompt import has no synthesis child".into(),
        );
    };
    if synthesis_status != "completed" {
        return Err(
            "video.prompt_history_mismatch: Completed prompt import has no completed History child"
                .into(),
        );
    }
    let history = runtime
        .store
        .get_history_by_job_id(&synthesis_job_id)?
        .ok_or("video.prompt_history_mismatch: Completed prompt synthesis has no History item")?;
    let synthesis_request = prompt_synthesis_request(request, parent_job_id);
    validate_prompt_history(runtime, request, &synthesis_request, &history)?;
    let history_id = value_string(&history, "id")?;
    let audio_path = history
        .get("audio_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or("video.prompt_history_mismatch: Completed prompt synthesis has no audio path")?;
    let mut project = runtime
        .video
        .get_project(&request.project_id)
        .map_err(service_error)?;

    if matches!(import_status.as_str(), "queued" | "preparing" | "running") {
        match runtime
            .video
            .wait_for_job(&import_job_id, &request.project_id, VIDEO_COMMAND_TIMEOUT)
        {
            Ok(completed) => {
                project = completed.project;
                import_status = "completed".into();
            }
            Err(error) => {
                import_status = runtime
                    .store
                    .job_status(&import_job_id)?
                    .ok_or("video.job_not_found: The durable prompt import disappeared")?;
                if !matches!(import_status.as_str(), "failed" | "cancelled") {
                    return Err(service_error(error));
                }
                project = runtime
                    .video
                    .get_project(&request.project_id)
                    .map_err(service_error)?;
            }
        }
    }

    if matches!(import_status.as_str(), "failed" | "cancelled") {
        if validate_prompt_imported_project(&project, request, &history_id, &synthesis_job_id)
            .is_err()
        {
            // A failed child that left the exact atomic draft untouched can resume through the
            // normal path. Any other revision is an unrelated/corrupt edit and remains closed.
            ensure_prompt_project(runtime, request)?;
            return Ok(None);
        }
        ensure_command_job_active(runtime, parent_job_id)?;
        let queued = runtime
            .video
            .queue_local_import_idempotent(
                prompt_local_import_request(request, audio_path),
                parent_job_id,
                None,
            )
            .map_err(service_error)?;
        if queued.job_id != import_job_id {
            runtime.video.cancel_job(&queued.job_id).ok();
            return Err(
                "video.resume_conflict: Prompt recovery selected a different import child".into(),
            );
        }
        if let Err(error) = ensure_command_job_active(runtime, parent_job_id) {
            runtime.video.cancel_job(&import_job_id).ok();
            return Err(error);
        }
        let completed = runtime
            .video
            .wait_for_job(&import_job_id, &request.project_id, VIDEO_COMMAND_TIMEOUT)
            .map_err(service_error)?;
        project = completed.project;
        import_status = "completed".into();
    }
    if import_status != "completed" {
        return Err(format!(
            "video.job_state_invalid: The prompt import child entered {import_status}"
        ));
    }
    validate_prompt_imported_project(&project, request, &history_id, &synthesis_job_id)?;
    ensure_command_job_active(runtime, parent_job_id)?;
    if !runtime.store.complete_job(parent_job_id)? {
        return Err(
            "video.parent_job_inactive: The prompt parent could not adopt its completed import"
                .into(),
        );
    }
    emit_operation_progress(
        progress,
        parent_job_id,
        &request.project_id,
        "source",
        1.0,
        "Recovered the registered prompt narration without generating it again",
        None,
    );
    Ok(Some(project))
}

fn prompt_local_import_request(
    request: &DurablePromptVideoRequest,
    audio_path: PathBuf,
) -> video::LocalImportRequest {
    video::LocalImportRequest {
        project_id: request.project_id.clone(),
        source_path: audio_path,
        actor: request.actor.clone(),
        title: Some(format!("{} narration", request.title)),
    }
}

fn validate_prompt_imported_project(
    project: &Value,
    request: &DurablePromptVideoRequest,
    history_id: &str,
    synthesis_job_id: &str,
) -> Result<(), String> {
    if project.get("revision").and_then(Value::as_i64) != Some(2) {
        return Err(
            "video.prompt_import_mismatch: Prompt recovery requires the exact first imported revision"
                .into(),
        );
    }
    let manifest: video::VideoProjectManifest = serde_json::from_value(
        project
            .get("manifest")
            .cloned()
            .ok_or("video.invalid_manifest: Prompt video manifest is missing")?,
    )
    .map_err(|error| format!("video.invalid_manifest: {error}"))?;
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())?;
    if manifest.revision != 2 || manifest.revision_history.len() != 2 {
        return Err(
            "video.prompt_import_mismatch: Prompt recovery found a later or incomplete timeline revision"
                .into(),
        );
    }
    let matching = manifest
        .source_assets
        .iter()
        .filter(|asset| {
            asset.kind == video::SourceAssetKind::SoundArSpeech
                && asset.provenance.kind == video::ProvenanceKind::GeneratedLocally
                && asset
                    .provenance
                    .metadata
                    .get("history_id")
                    .and_then(Value::as_str)
                    == Some(history_id)
                && asset
                    .provenance
                    .metadata
                    .get("generation_job_id")
                    .and_then(Value::as_str)
                    == Some(synthesis_job_id)
                && asset
                    .provenance
                    .metadata
                    .get("model_id")
                    .and_then(Value::as_str)
                    == Some(request.model_id.as_str())
        })
        .collect::<Vec<_>>();
    if manifest.source_assets.len() != 1 || matching.len() != 1 {
        return Err(
            "video.prompt_import_mismatch: Prompt narration was not imported exactly once with its History provenance"
                .into(),
        );
    }
    let source = matching[0];
    let registered = project
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|asset| asset.get("id").and_then(Value::as_str) == Some(source.id.as_str()))
        })
        .ok_or(
            "video.prompt_import_mismatch: Prompt narration has no registered source media record",
        )?;
    let provenance = registered.get("provenance").unwrap_or(&Value::Null);
    let provenance_metadata = provenance.get("metadata").unwrap_or(&Value::Null);
    if registered.get("project_id").and_then(Value::as_str) != Some(request.project_id.as_str())
        || registered.get("kind").and_then(Value::as_str) != Some("speech")
        || registered.get("status").and_then(Value::as_str) != Some("ready")
        || registered.get("content_sha256").and_then(Value::as_str) != Some(source.sha256.as_str())
        || provenance_metadata
            .get("history_id")
            .and_then(Value::as_str)
            != Some(history_id)
        || provenance_metadata
            .get("generation_job_id")
            .and_then(Value::as_str)
            != Some(synthesis_job_id)
        || provenance_metadata.get("model_id").and_then(Value::as_str)
            != Some(request.model_id.as_str())
    {
        return Err(
            "video.prompt_import_mismatch: Prompt narration media is not ready with its exact History provenance"
                .into(),
        );
    }
    Ok(())
}

fn installed_tts_route(runtime: &RuntimeState) -> Option<(String, String, String)> {
    let registry = read_json(runtime.model_registry_path.clone(), json!({ "models": [] }));
    let models = registry.get("models")?.as_array()?;
    let is_ready = |model: &Value| {
        model.get("task").and_then(Value::as_str) == Some("tts")
            && model
                .pointer("/integrity/state")
                .and_then(Value::as_str)
                .is_some_and(|state| state == "ready")
            && model
                .get("local_path")
                .and_then(Value::as_str)
                .is_some_and(|path| Path::new(path).exists())
    };
    let selected = models
        .iter()
        .find(|model| {
            is_ready(model)
                && model.get("model_id").and_then(Value::as_str) == Some("hexgrad/Kokoro-82M")
        })
        .or_else(|| models.iter().find(|model| is_ready(model)))?;
    let model_id = selected.get("model_id")?.as_str()?.to_string();
    let engine = selected.get("engine").and_then(Value::as_str).unwrap_or("");
    let (speaker, voice_name) = if engine == "kokoro" {
        ("af_heart", "Heart")
    } else {
        ("default", "Default voice")
    };
    Some((model_id, speaker.into(), voice_name.into()))
}

fn advance_manifest_revision(
    manifest: &mut video::VideoProjectManifest,
    actor: &str,
    reason: &str,
    changed_paths: Vec<String>,
    invalidated_stages: BTreeSet<video::RevisionStage>,
) -> Result<(), String> {
    let parent_id = manifest
        .revision_history
        .last()
        .map(|record| record.id.clone());
    manifest.revision = manifest
        .revision
        .checked_add(1)
        .ok_or("video.invalid_revision: Revision overflow")?;
    let updated_at = utc_now();
    manifest.updated_at = updated_at.clone();
    manifest.revision_history.push(video::RevisionRecord {
        id: Uuid::new_v4().simple().to_string(),
        revision: manifest.revision,
        parent_id,
        actor: actor.into(),
        reason: reason.into(),
        changed_paths,
        invalidated_stages,
        created_at: updated_at,
    });
    manifest
        .validate_strict()
        .map_err(|error| error.to_string())
}

fn candidate_policy(duration: video::Microseconds) -> video::CandidatePolicy {
    if duration.0 >= 12_000_000 {
        video::CandidatePolicy::default()
    } else {
        let minimum = (duration.0 / 4).clamp(250_000, 2_000_000);
        let target = (duration.0 * 2 / 3).max(minimum);
        video::CandidatePolicy {
            minimum_duration_us: video::Microseconds(minimum),
            target_duration_us: video::Microseconds(target),
            maximum_duration_us: duration,
            maximum_candidates: 6,
        }
    }
}

fn bundled_whisper_ready(runtime: &RuntimeState) -> bool {
    installed_whisper_model(runtime).is_some()
}

fn installed_whisper_model(runtime: &RuntimeState) -> Option<String> {
    let registry = read_json(runtime.model_registry_path.clone(), json!({ "models": [] }));
    select_ready_whisper_model(&registry)
}

fn select_ready_whisper_model(registry: &Value) -> Option<String> {
    let models = registry.get("models")?.as_array()?;
    [
        "openai/whisper-large-v3-turbo",
        "openai/whisper-small",
        "openai/whisper-tiny",
    ]
    .iter()
    .find_map(|preferred| {
        models
            .iter()
            .find(|model| {
                model.get("model_id").and_then(Value::as_str) == Some(*preferred)
                    && registry_model_ready_for_task(model, "stt")
            })
            .map(|_| (*preferred).to_string())
    })
    .or_else(|| {
        models.iter().find_map(|model| {
            let id = model.get("model_id").and_then(Value::as_str)?;
            (id.to_ascii_lowercase().contains("whisper")
                && registry_model_ready_for_task(model, "stt"))
            .then(|| id.to_string())
        })
    })
}

fn registry_model_ready_for_task(model: &Value, task: &str) -> bool {
    model.get("task").and_then(Value::as_str) == Some(task)
        && model.pointer("/integrity/state").and_then(Value::as_str) == Some("ready")
        && model
            .get("local_path")
            .and_then(Value::as_str)
            .is_some_and(|path| Path::new(path).exists())
}

fn progress_callback(
    app: tauri::AppHandle,
    fallback_phase: &'static str,
) -> video::ProgressCallback {
    Arc::new(move |progress| {
        let phase = match progress.phase.as_str() {
            "validating" | "copying" | "downloading" | "downloaded" | "source_ready" => "source",
            "proxy_ready" | "thumbnail_ready" | "waveform_ready" => "analyze",
            "analyze" => "analyze",
            "review" => "review",
            "rendering" | "publishing" | "completed" => fallback_phase,
            _ => fallback_phase,
        };
        emit_ui_progress(
            &app,
            &progress.job_id,
            &progress.project_id,
            phase,
            if progress.phase == "completed" {
                "completed"
            } else if progress.phase == "failed" {
                "failed"
            } else {
                "running"
            },
            progress.progress,
            &progress.message,
            progress.playable_artifact,
        );
    })
}

fn emit_operation_progress(
    callback: Option<&video::ProgressCallback>,
    job_id: &str,
    project_id: &str,
    phase: &str,
    progress: f64,
    message: &str,
    playable_artifact: Option<Value>,
) {
    if let Some(callback) = callback {
        callback(video::VideoServiceProgress {
            job_id: job_id.to_string(),
            project_id: project_id.to_string(),
            phase: phase.to_string(),
            progress: progress.clamp(0.0, 1.0),
            message: message.to_string(),
            playable_artifact,
            metrics: None,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_ui_progress(
    app: &tauri::AppHandle,
    job_id: &str,
    project_id: &str,
    phase: &str,
    status: &str,
    progress: f64,
    detail: &str,
    partial_artifact: Option<Value>,
) {
    let timestamp = utc_now();
    let _ = app.emit(
        "video-job-progress",
        json!({
            "job": {
                "id": job_id,
                "project_id": project_id,
                "phase": phase,
                "status": status,
                "progress": progress.clamp(0.0, 1.0),
                "title": phase_title(phase),
                "detail": detail,
                "durable": true,
                "created_at": timestamp,
                "updated_at": timestamp,
            },
            "partial_artifact": partial_artifact,
        }),
    );
}

fn phase_title(phase: &str) -> &'static str {
    match phase {
        "source" => "Preparing source",
        "analyze" => "Analyzing source",
        "review" => "Planning scenes",
        "preview" => "Rendering preview",
        "export" => "Exporting video",
        _ => "Video Studio",
    }
}

fn prompt_title(prompt: &str) -> String {
    let title = prompt
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "Animated audio · Video draft".into()
    } else {
        bounded_name(&format!("{title} · Video draft"), "Video draft")
    }
}

fn bounded_name(value: &str, fallback: &str) -> String {
    let value = value.trim();
    let value = if value.is_empty() { fallback } else { value };
    value.chars().take(160).collect()
}

fn value_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("video.invalid_result: Service result is missing {field}"))
}

fn service_error(error: video::VideoServiceError) -> String {
    error.stable_message()
}

fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn write_test_pcm_wav(path: &Path, duration_ms: u32) -> String {
        const SAMPLE_RATE: u32 = 8_000;
        const CHANNELS: u16 = 1;
        const BITS_PER_SAMPLE: u16 = 16;
        let sample_count = SAMPLE_RATE.saturating_mul(duration_ms) / 1_000;
        let data_size = sample_count.saturating_mul(u32::from(BITS_PER_SAMPLE / 8));
        let mut bytes = Vec::with_capacity(44 + usize::try_from(data_size).unwrap_or_default());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36_u32.saturating_add(data_size)).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&CHANNELS.to_le_bytes());
        bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        let byte_rate = SAMPLE_RATE
            .saturating_mul(u32::from(CHANNELS))
            .saturating_mul(u32::from(BITS_PER_SAMPLE / 8));
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = CHANNELS.saturating_mul(BITS_PER_SAMPLE / 8);
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.resize(44 + usize::try_from(data_size).unwrap_or_default(), 0);
        fs::create_dir_all(path.parent().expect("test WAV parent"))
            .expect("create test WAV parent");
        fs::write(path, &bytes).expect("write test PCM WAV");
        format!("{:x}", Sha256::digest(&bytes))
    }

    fn copy_test_directory(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create copied test directory");
        for entry in fs::read_dir(source).expect("read copied test directory") {
            let entry = entry.expect("read copied test entry");
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if entry
                .file_type()
                .expect("inspect copied test entry")
                .is_dir()
            {
                copy_test_directory(&source_path, &target_path);
            } else {
                fs::copy(&source_path, &target_path).expect("copy test state file");
            }
        }
    }

    fn revision_fixture() -> video::VideoProjectManifest {
        let mut manifest = video::VideoProjectManifest::new(
            "revision-project",
            "Revision project",
            video::RationalFrameRate::FPS_30,
            video::Microseconds(4_000_000),
            video::LayoutPlan {
                mode: video::CanvasMode::Portrait,
                canvas: video::CanvasSpec {
                    width: 1080,
                    height: 1920,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: video::NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 9_000,
                    height_bp: 9_000,
                },
                background_rgba: [24, 24, 24, 255],
                elements: vec![],
            },
            video::AudioMix {
                target_lufs_milli: -14_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            "2026-08-27T20:00:00.000Z",
        )
        .unwrap();
        manifest.source_assets.push(video::SourceAsset {
            id: "source-1".into(),
            kind: video::SourceAssetKind::LocalVideo,
            managed_path: "projects/revision-project/source.mp4".into(),
            sha256: "a".repeat(64),
            probe: video::MediaProbe {
                duration_us: video::Microseconds(5_000_000),
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(video::RationalFrameRate::FPS_30),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: video::Provenance {
                kind: video::ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: "2026-08-27T20:00:00.000Z".into(),
                producer: "revision-test".into(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        manifest.reviewed_scenes = vec![
            video::ReviewedScene {
                id: "scene-opening".into(),
                candidate_id: None,
                source_asset_id: Some("source-1".into()),
                source_range: Some(video::TimeRange::new(0, 2_000_000).unwrap()),
                timeline_start_us: video::Microseconds::ZERO,
                timeline_duration_us: video::Microseconds(2_000_000),
                title: "Opening".into(),
                script: "A concise opening statement.".into(),
                review_state: video::ReviewState::Approved,
                revision: 1,
            },
            video::ReviewedScene {
                id: "scene-close".into(),
                candidate_id: None,
                source_asset_id: Some("source-1".into()),
                source_range: Some(video::TimeRange::new(3_000_000, 5_000_000).unwrap()),
                timeline_start_us: video::Microseconds(2_000_000),
                timeline_duration_us: video::Microseconds(2_000_000),
                title: "Close".into(),
                script: "A clear final takeaway.".into(),
                review_state: video::ReviewState::Approved,
                revision: 1,
            },
        ];
        let clips = |prefix: &str| {
            vec![
                video::TimelineClip {
                    id: format!("{prefix}-opening"),
                    scene_id: Some("scene-opening".into()),
                    media: video::MediaReference {
                        source_asset_id: Some("source-1".into()),
                        render_artifact_id: None,
                    },
                    source_range: video::TimeRange::new(0, 2_000_000).unwrap(),
                    timeline_start_us: video::Microseconds::ZERO,
                    timeline_duration_us: video::Microseconds(2_000_000),
                    playback_rate: video::RationalRate::ONE,
                    gain_db_milli: 0,
                    muted: false,
                    crop: None,
                },
                video::TimelineClip {
                    id: format!("{prefix}-close"),
                    scene_id: Some("scene-close".into()),
                    media: video::MediaReference {
                        source_asset_id: Some("source-1".into()),
                        render_artifact_id: None,
                    },
                    source_range: video::TimeRange::new(3_000_000, 5_000_000).unwrap(),
                    timeline_start_us: video::Microseconds(2_000_000),
                    timeline_duration_us: video::Microseconds(2_000_000),
                    playback_rate: video::RationalRate::ONE,
                    gain_db_milli: 0,
                    muted: false,
                    crop: None,
                },
            ]
        };
        manifest.tracks = vec![
            video::TimelineTrack {
                id: "video-main".into(),
                kind: video::TrackKind::Video,
                clips: clips("video"),
                preserve_gaps: true,
            },
            video::TimelineTrack {
                id: "audio-main".into(),
                kind: video::TrackKind::Audio,
                clips: clips("audio"),
                preserve_gaps: true,
            },
        ];
        manifest.audio_mix.tracks.push(video::AudioMixTrack {
            track_id: "audio-main".into(),
            gain_db_milli: 0,
            pan_milli: 0,
            ducking: None,
        });
        manifest.captions = vec![
            video::CaptionCue {
                id: "caption-opening".into(),
                range: video::TimeRange::new(0, 2_000_000).unwrap(),
                text: "A concise opening statement.".into(),
                style_id: "caption-clean-white".into(),
                speaker_id: None,
                transcript_segment_id: None,
                scene_id: Some("scene-opening".into()),
            },
            video::CaptionCue {
                id: "caption-close".into(),
                range: video::TimeRange::new(2_000_000, 4_000_000).unwrap(),
                text: "A clear final takeaway.".into(),
                style_id: "caption-clean-white".into(),
                speaker_id: None,
                transcript_segment_id: None,
                scene_id: Some("scene-close".into()),
            },
        ];
        manifest.validate_strict().unwrap();
        manifest
    }

    #[test]
    fn short_sources_receive_valid_candidate_policy() {
        let short = candidate_policy(video::Microseconds(3_000_000));
        short.validate().unwrap();
        assert_eq!(short.maximum_duration_us, video::Microseconds(3_000_000));
        let regular = candidate_policy(video::Microseconds(60_000_000));
        assert_eq!(regular, video::CandidatePolicy::default());
    }

    #[test]
    fn whisper_selection_requires_stt_task_ready_integrity_and_local_payload() {
        let root = std::env::temp_dir().join(format!("soundar-whisper-route-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create model fixture");
        let ready_path = root.join("whisper-small");
        fs::create_dir_all(&ready_path).expect("create ready model payload");
        let registry = json!({"models": [
            {
                "model_id": "openai/whisper-large-v3-turbo",
                "task": "tts",
                "integrity": {"state": "ready"},
                "local_path": ready_path,
            },
            {
                "model_id": "openai/whisper-tiny",
                "task": "stt",
                "integrity": {"state": "failed"},
                "local_path": ready_path,
            },
            {
                "model_id": "openai/whisper-small",
                "task": "stt",
                "integrity": {"state": "ready"},
                "local_path": ready_path,
            }
        ]});
        assert_eq!(
            select_ready_whisper_model(&registry).as_deref(),
            Some("openai/whisper-small")
        );

        let missing = json!({"models": [{
            "model_id": "openai/whisper-small",
            "task": "stt",
            "integrity": {"state": "ready"},
            "local_path": root.join("missing"),
        }]});
        assert!(select_ready_whisper_model(&missing).is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn revision_helper_is_contiguous_and_strict() {
        let mut manifest = video::VideoProjectManifest::new(
            "project-1",
            "Project",
            video::RationalFrameRate::FPS_30,
            video::Microseconds(1_000_000),
            video::LayoutPlan {
                mode: video::CanvasMode::Portrait,
                canvas: video::CanvasSpec {
                    width: 1080,
                    height: 1920,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: video::NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 9_000,
                    height_bp: 9_000,
                },
                background_rgba: [24, 24, 24, 255],
                elements: vec![],
            },
            video::AudioMix {
                target_lufs_milli: -14_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            utc_now(),
        )
        .unwrap();
        advance_manifest_revision(
            &mut manifest,
            "test",
            "Create",
            vec!["/".into()],
            BTreeSet::new(),
        )
        .unwrap();
        advance_manifest_revision(
            &mut manifest,
            "test",
            "Change captions",
            vec!["/captions".into()],
            BTreeSet::from([video::RevisionStage::Preview]),
        )
        .unwrap();
        assert_eq!(manifest.revision, 2);
        assert_eq!(
            manifest.revision_history[1].parent_id.as_deref(),
            Some(manifest.revision_history[0].id.as_str())
        );
        manifest.validate_strict().unwrap();
    }

    #[test]
    fn user_supplied_names_are_bounded_without_byte_truncation() {
        let name = bounded_name(&"é".repeat(200), "fallback");
        assert_eq!(name.chars().count(), 160);
        assert!(name.is_char_boundary(name.len()));
    }

    #[test]
    fn durable_analysis_and_plan_requests_round_trip_exact_project_expectations() {
        let analyze = DurableAnalyzeRequest {
            project_id: "project-1".into(),
            source_asset_id: "source-1".into(),
            source_sha256: "a".repeat(64),
            model_id: "openai/whisper-small".into(),
            language: Some("en".into()),
            expected_revision: 7,
            expected_version_id: "version-7".into(),
            priority: "normal".into(),
            title: "Analyze project".into(),
        };
        let analyze_value = serde_json::to_value(&analyze).unwrap();
        let decoded: DurableAnalyzeRequest = serde_json::from_value(analyze_value).unwrap();
        assert_eq!(decoded.expected_revision, 7);
        assert_eq!(decoded.expected_version_id, "version-7");
        assert_eq!(decoded.source_sha256, "a".repeat(64));

        let plan = DurablePlanRequest {
            project_id: "project-1".into(),
            selected_candidate_ids: vec!["candidate-1".into()],
            creative_brief: Some("Keep the opening restrained".into()),
            expected_revision: 8,
            expected_version_id: "version-8".into(),
            priority: "normal".into(),
            title: "Plan project".into(),
        };
        let plan_value = serde_json::to_value(&plan).unwrap();
        let decoded: DurablePlanRequest = serde_json::from_value(plan_value).unwrap();
        assert_eq!(decoded.selected_candidate_ids, ["candidate-1"]);
        assert_eq!(decoded.expected_revision, 8);
        assert_eq!(decoded.expected_version_id, "version-8");
    }

    #[test]
    fn durable_narration_request_binds_route_script_reference_and_parent_child_payload() {
        let durable = DurableNarrationRevisionRequest {
            project_id: "project-1".into(),
            scene_id: "scene-opening".into(),
            binding_id: Some("binding-opening".into()),
            script: "A reviewed opening.".into(),
            script_sha256: sha256_text("A reviewed opening."),
            voice_id: "voice-consented".into(),
            model_id: "local/chatterbox".into(),
            speaker: "default".into(),
            language: "en-US".into(),
            voice_name: "Consented narrator".into(),
            reference_audio_path: Some("/managed/voice-reference.wav".into()),
            expected_revision: 9,
            expected_version_id: "version-9".into(),
            actor: "codex-agent".into(),
            priority: "normal".into(),
            title: "Regenerate narration".into(),
        };
        let encoded = serde_json::to_value(&durable).unwrap();
        let decoded: DurableNarrationRevisionRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, durable);

        let child = narration_synthesis_request(&decoded, "parent-job-1");
        assert_eq!(child["video_parent_job_id"], "parent-job-1");
        assert_eq!(child["video_project_id"], "project-1");
        assert_eq!(child["video_scene_id"], "scene-opening");
        assert_eq!(
            child["reference_audio_path"],
            "/managed/voice-reference.wav"
        );
        assert_eq!(child["text"], "A reviewed opening.");
        assert_eq!(child["model_id"], "local/chatterbox");
    }

    #[test]
    fn narration_accepts_fresh_and_reloaded_integrity_states_only() {
        assert!(narration_history_artifact_is_intact(
            &json!({"artifact_state": "verified"})
        ));
        assert!(narration_history_artifact_is_intact(
            &json!({"artifact_state": "available"})
        ));
        for rejected in ["missing", "modified", "unknown", "ready"] {
            assert!(!narration_history_artifact_is_intact(
                &json!({"artifact_state": rejected})
            ));
        }
    }

    #[test]
    fn narration_recovery_requires_the_exact_history_job_and_artifact_provenance() {
        let mut manifest = revision_fixture();
        advance_manifest_revision(
            &mut manifest,
            "test-suite",
            "Reviewed base",
            vec!["/".into()],
            BTreeSet::new(),
        )
        .expect("advance reviewed base");
        let script = manifest.reviewed_scenes[0].script.clone();
        let artifact = video::RenderArtifact {
            id: "narration-artifact-exact".into(),
            role: video::RenderArtifactRole::SceneSegment,
            scene_id: Some("scene-opening".into()),
            managed_path: "projects/revision-project/narration-exact.wav".into(),
            sha256: "b".repeat(64),
            cache_key: "c".repeat(64),
            mime_type: "audio/wav".into(),
            duration_us: Some(video::Microseconds(2_000_000)),
            width: None,
            height: None,
            publication_state: video::PublicationState::Published,
            created_at: "2026-08-27T20:00:00.000Z".into(),
        };
        manifest.render_artifacts.push(artifact.clone());
        manifest.narration_bindings.push(video::NarrationBinding {
            id: "binding-opening".into(),
            scene_id: Some("scene-opening".into()),
            render_artifact_id: artifact.id.clone(),
            history_id: "history-exact".into(),
            generation_job_id: "synthesis-exact".into(),
            voice_id: "af_heart".into(),
            model_id: "hexgrad/Kokoro-82M".into(),
            speaker: "af_heart".into(),
            language: "en-US".into(),
            script_sha256: sha256_text(&script),
            created_at: "2026-08-27T20:00:00.000Z".into(),
        });
        advance_manifest_revision(
            &mut manifest,
            "test-suite",
            "Regenerated opening",
            vec!["/render_artifacts".into(), "/narration_bindings".into()],
            BTreeSet::from([
                video::RevisionStage::Speech,
                video::RevisionStage::SceneRender,
                video::RevisionStage::Preview,
                video::RevisionStage::FinalRender,
                video::RevisionStage::PublishPackage,
            ]),
        )
        .expect("advance exact narration result");
        manifest
            .validate_strict()
            .expect("valid exact narration result");
        let request = DurableNarrationRevisionRequest {
            project_id: manifest.project_id.clone(),
            scene_id: "scene-opening".into(),
            binding_id: Some("binding-opening".into()),
            script: script.clone(),
            script_sha256: sha256_text(&script),
            voice_id: "af_heart".into(),
            model_id: "hexgrad/Kokoro-82M".into(),
            speaker: "af_heart".into(),
            language: "en-US".into(),
            voice_name: "Heart".into(),
            reference_audio_path: None,
            expected_revision: 1,
            expected_version_id: "version-one".into(),
            actor: "test-suite".into(),
            priority: "normal".into(),
            title: "Regenerate opening".into(),
        };
        let asset = json!({
            "id": artifact.id,
            "project_id": manifest.project_id,
            "kind": "speech",
            "source_kind": "derived",
            "status": "ready",
            "content_sha256": artifact.sha256,
            "provenance": {
                "history_id": "history-exact",
                "generation_job_id": "synthesis-exact",
                "voice_id": request.voice_id,
                "model_id": request.model_id,
                "speaker": request.speaker,
                "language": request.language,
            }
        });
        let project = json!({"revision": 2, "manifest": manifest, "assets": [asset]});
        validate_committed_narration_result(&project, &request, "history-exact", "synthesis-exact")
            .expect("adopt exact durable narration take");

        let mut later_revision = project.clone();
        later_revision["revision"] = json!(3);
        assert!(validate_committed_narration_result(
            &later_revision,
            &request,
            "history-exact",
            "synthesis-exact",
        )
        .expect_err("reject an exact take retained by a later unrelated edit")
        .starts_with("video.narration_result_missing:"));

        let mut stale_take = project.clone();
        stale_take["manifest"]["narration_bindings"][0]["history_id"] =
            json!("history-later-same-route");
        stale_take["manifest"]["narration_bindings"][0]["generation_job_id"] =
            json!("synthesis-later-same-route");
        assert!(validate_committed_narration_result(
            &stale_take,
            &request,
            "history-exact",
            "synthesis-exact",
        )
        .expect_err("reject a later take with identical route and script")
        .starts_with("video.narration_result_changed:"));

        let mut stale_asset = project;
        stale_asset["assets"][0]["provenance"]["history_id"] = json!("history-other");
        assert!(validate_committed_narration_result(
            &stale_asset,
            &request,
            "history-exact",
            "synthesis-exact",
        )
        .expect_err("reject mismatched conformed artifact provenance")
        .starts_with("video.narration_result_changed:"));
    }

    #[test]
    fn narration_parent_cancellation_wins_the_terminal_completion_race() {
        let root = std::env::temp_dir().join(format!(
            "soundar-narration-terminal-race-{}",
            Uuid::new_v4()
        ));
        let store = crate::store::Store::open(root.join("data"), root.join("artifacts"))
            .expect("open narration race store");
        let parent = store
            .create_job(
                "video_regenerate_narration",
                &json!({"project_id": "narration-race-project"}),
            )
            .expect("create narration parent");
        store.start_job(&parent).expect("start narration parent");
        assert!(store
            .cancel_job(&parent)
            .expect("cancel before terminal CAS"));
        let runtime =
            RuntimeState::new_with_store(root.join("runtime"), PathBuf::from("python3"), store);
        let error = complete_narration_parent(&runtime, &parent)
            .expect_err("cancelled parent must never report narration success");
        assert!(
            error.starts_with("video.cancelled:"),
            "unexpected error: {error}"
        );
        assert_eq!(
            runtime
                .store
                .job_status(&parent)
                .expect("read terminal narration state")
                .as_deref(),
            Some("cancelled")
        );
        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn narration_parent_rearms_exact_child_after_post_manifest_restart() {
        let root = std::env::temp_dir().join(format!(
            "soundar-narration-post-manifest-restart-{}",
            Uuid::new_v4()
        ));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let store = crate::store::Store::open(data.clone(), artifacts.clone())
            .expect("open narration post-manifest store");
        let runtime = RuntimeState::new_with_store(
            root.join("runtime-first"),
            PathBuf::from("python3"),
            store,
        );
        let initial_manifest = revision_fixture();
        let project_id = initial_manifest.project_id.clone();
        let initial = runtime
            .video
            .create_project(video::service::CreateVideoProjectRequest {
                name: initial_manifest.name.clone(),
                manifest: initial_manifest,
                actor: "test-suite".into(),
                initial_intent: Some("Verify narration crash recovery".into()),
            })
            .expect("create narration recovery project");
        let expected_version_id = project_version_id(&initial)
            .expect("initial narration version")
            .to_string();
        let base_manifest: video::VideoProjectManifest =
            serde_json::from_value(initial["manifest"].clone()).expect("decode narration base");
        let script = base_manifest.reviewed_scenes[0].script.clone();
        let request = DurableNarrationRevisionRequest {
            project_id: project_id.clone(),
            scene_id: "scene-opening".into(),
            binding_id: Some("binding-opening".into()),
            script: script.clone(),
            script_sha256: sha256_text(&script),
            voice_id: "voice-recovery".into(),
            model_id: "test/narration-model".into(),
            speaker: "default".into(),
            language: "en".into(),
            voice_name: "Recovery narrator".into(),
            reference_audio_path: None,
            expected_revision: 1,
            expected_version_id: expected_version_id.clone(),
            actor: "test-suite".into(),
            priority: "normal".into(),
            title: "Regenerate opening".into(),
        };
        let parent_job_id = runtime
            .store
            .create_job(
                "video_regenerate_narration",
                &serde_json::to_value(&request).expect("encode narration parent"),
            )
            .expect("create narration parent");
        runtime
            .store
            .start_job(&parent_job_id)
            .expect("start narration parent");

        let synthesis_request = narration_synthesis_request(&request, &parent_job_id);
        let (synthesis_job_id, created) = runtime
            .store
            .create_idempotent_job(
                "synthesis",
                &format!("video-narration-synthesis-{parent_job_id}"),
                &synthesis_request,
            )
            .expect("create exact narration synthesis")
            .expect("narration synthesis identity");
        assert!(created);
        runtime
            .store
            .start_job(&synthesis_job_id)
            .expect("start narration synthesis");
        let history_path = artifacts.join("narration-post-manifest-history.wav");
        write_test_pcm_wav(&history_path, 700);
        runtime
            .store
            .complete_synthesis(
                &synthesis_job_id,
                &synthesis_request,
                &json!({
                    "id": "narration-post-manifest-history",
                    "generation_kind": "speech",
                    "model_id": request.model_id,
                    "engine": "test-engine",
                    "audio_path": history_path,
                    "sample_rate": 8_000,
                    "duration_seconds": 0.7,
                    "inference_seconds": 0.01,
                    "rtf": 0.02,
                    "vram_peak_mb": 0,
                    "waveform": [0.0],
                }),
            )
            .expect("publish exact narration History");

        let replacement_request = narration_replacement_request(
            &request,
            &parent_job_id,
            "narration-post-manifest-history",
        );
        let (replacement_job_id, created) = runtime
            .store
            .create_idempotent_job(
                "video_replace_narration",
                &format!("narration-replacement:{parent_job_id}"),
                &serde_json::to_value(&replacement_request).expect("encode replacement child"),
            )
            .expect("create exact replacement child")
            .expect("replacement child identity");
        assert!(created);
        runtime
            .store
            .start_job(&replacement_job_id)
            .expect("start replacement child");

        let render_path = runtime
            .store
            .video_artifacts_root()
            .join("projects")
            .join(&project_id)
            .join("narration-post-manifest.wav");
        let render_sha = write_test_pcm_wav(&render_path, 2_000);
        let render_size = i64::try_from(
            fs::metadata(&render_path)
                .expect("inspect narration render")
                .len(),
        )
        .expect("narration render size");
        let artifact_id = "narration-post-manifest-artifact";
        let mut committed_manifest = base_manifest;
        let opening_clip = committed_manifest
            .tracks
            .iter_mut()
            .filter(|track| track.kind == video::TrackKind::Audio)
            .flat_map(|track| track.clips.iter_mut())
            .find(|clip| clip.scene_id.as_deref() == Some("scene-opening"))
            .expect("opening narration clip");
        opening_clip.media.source_asset_id = None;
        opening_clip.media.render_artifact_id = Some(artifact_id.into());
        opening_clip.source_range = video::TimeRange::new(0, 2_000_000).unwrap();
        opening_clip.playback_rate = video::RationalRate::ONE;
        committed_manifest
            .render_artifacts
            .push(video::RenderArtifact {
                id: artifact_id.into(),
                role: video::RenderArtifactRole::SceneSegment,
                scene_id: Some("scene-opening".into()),
                managed_path: format!("projects/{project_id}/narration-post-manifest.wav"),
                sha256: render_sha.clone(),
                cache_key: "d".repeat(64),
                mime_type: "audio/wav".into(),
                duration_us: Some(video::Microseconds(2_000_000)),
                width: None,
                height: None,
                publication_state: video::PublicationState::Published,
                created_at: utc_now(),
            });
        committed_manifest
            .narration_bindings
            .push(video::NarrationBinding {
                id: "binding-opening".into(),
                scene_id: Some("scene-opening".into()),
                render_artifact_id: artifact_id.into(),
                history_id: "narration-post-manifest-history".into(),
                generation_job_id: synthesis_job_id.clone(),
                voice_id: request.voice_id.clone(),
                model_id: request.model_id.clone(),
                speaker: request.speaker.clone(),
                language: request.language.clone(),
                script_sha256: request.script_sha256.clone(),
                created_at: utc_now(),
            });
        let changed_paths = vec![
            "/narration_bindings".into(),
            "/render_artifacts".into(),
            "/tracks".into(),
        ];
        let invalidated = BTreeSet::from([
            video::RevisionStage::Speech,
            video::RevisionStage::SceneRender,
            video::RevisionStage::Preview,
            video::RevisionStage::FinalRender,
            video::RevisionStage::PublishPackage,
        ]);
        advance_manifest_revision(
            &mut committed_manifest,
            &request.actor,
            "Regenerated selected narration with a revised voice",
            changed_paths,
            invalidated,
        )
        .expect("advance exact narration commit");
        runtime
            .store
            .upsert_video_asset(&json!({
                "id": artifact_id,
                "project_id": project_id,
                "kind": "speech",
                "source_kind": "derived",
                "local_path": render_path,
                "mime_type": "audio/wav",
                "content_sha256": render_sha,
                "size_bytes": render_size,
                "duration_us": 2_000_000,
                "status": "ready",
                "probe": {"duration_us": 2_000_000, "has_audio": true},
                "provenance": {
                    "history_id": "narration-post-manifest-history",
                    "generation_job_id": synthesis_job_id,
                    "voice_id": request.voice_id,
                    "model_id": request.model_id,
                    "speaker": request.speaker,
                    "language": request.language,
                },
            }))
            .expect("register exact narration artifact");
        let lease = runtime
            .store
            .acquire_video_project_lock(&project_id, "test-suite", 60)
            .expect("lock narration project");
        let token = lease["token"].as_str().expect("lock token");
        let committed = runtime
            .store
            .commit_video_manifest(
                &project_id,
                1,
                &serde_json::to_value(&committed_manifest).expect("encode committed narration"),
                &request.actor,
                "Regenerated selected narration with a revised voice",
                token,
                Some("ready"),
            )
            .expect("commit narration before worker terminal state");
        runtime
            .store
            .release_video_project_lock(&project_id, token)
            .expect("release narration project lock");
        validate_committed_narration_result(
            &committed,
            &request,
            "narration-post-manifest-history",
            &synthesis_job_id,
        )
        .expect("exact committed narration result");
        let committed_version = project_version_id(&committed)
            .expect("committed narration version")
            .to_string();
        drop(runtime);
        let stale_data = root.join("stale-data");
        copy_test_directory(&data, &stale_data);

        let reopened = crate::store::Store::open(data, artifacts.clone())
            .expect("reopen after narration worker crash");
        assert_eq!(
            reopened
                .job_status(&replacement_job_id)
                .expect("read interrupted replacement")
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            reopened
                .job_status(&parent_job_id)
                .expect("read interrupted narration parent")
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            reopened
                .latest_video_project_job(&project_id)
                .expect("resolve narration recovery owner")
                .expect("recoverable narration parent")["id"],
            parent_job_id
        );
        reopened
            .resume_video_job(&parent_job_id, &["video_regenerate_narration"])
            .expect("rearm narration parent");
        let resumed = RuntimeState::new_with_store(
            root.join("runtime-resumed"),
            PathBuf::from("python3"),
            reopened,
        );
        let recovered =
            run_narration_revision_job_guarded(&resumed, &parent_job_id, request.clone(), None)
                .expect("adopt post-manifest narration child");
        assert_eq!(recovered["revision"], 2);
        assert_eq!(
            project_version_id(&recovered).expect("recovered narration version"),
            committed_version
        );
        assert_eq!(
            recovered["manifest"]["narration_bindings"]
                .as_array()
                .expect("narration bindings")
                .len(),
            1
        );
        assert_eq!(
            recovered["manifest"]["revision_history"]
                .as_array()
                .expect("revision history")
                .len(),
            2
        );
        assert_eq!(
            resumed
                .store
                .video_child_job(&parent_job_id, "video_replace_narration")
                .expect("reload replacement child")
                .expect("bound replacement child"),
            (replacement_job_id.clone(), "completed".into())
        );
        assert_eq!(
            resumed
                .store
                .job_status(&parent_job_id)
                .expect("read completed narration parent")
                .as_deref(),
            Some("completed")
        );

        let stale_store = crate::store::Store::open(stale_data, artifacts)
            .expect("open stale narration crash snapshot");
        let stale_runtime = RuntimeState::new_with_store(
            root.join("runtime-stale"),
            PathBuf::from("python3"),
            stale_store,
        );
        let stale_current = stale_runtime
            .video
            .get_project(&project_id)
            .expect("load narration result before unrelated edit");
        let mut stale_manifest: video::VideoProjectManifest =
            serde_json::from_value(stale_current["manifest"].clone())
                .expect("decode narration result before unrelated edit");
        stale_manifest.name.push_str(" · later edit");
        advance_manifest_revision(
            &mut stale_manifest,
            "other-editor",
            "Rename after narration replacement",
            vec!["/name".into()],
            BTreeSet::from([video::RevisionStage::PublishPackage]),
        )
        .expect("advance unrelated narration edit");
        stale_runtime
            .video
            .revise_manifest(video::service::ReviseVideoManifestRequest {
                project_id: project_id.clone(),
                expected_revision: 2,
                manifest: stale_manifest,
                actor: "other-editor".into(),
                reason: "Rename after narration replacement".into(),
                changed_paths: vec!["/name".into()],
                invalidated_stages: BTreeSet::from([video::RevisionStage::PublishPackage]),
                status: Some("ready".into()),
            })
            .expect("commit unrelated narration edit");
        stale_runtime
            .store
            .resume_video_job(&parent_job_id, &["video_regenerate_narration"])
            .expect("rearm stale narration parent");
        let error =
            run_narration_revision_job_guarded(&stale_runtime, &parent_job_id, request, None)
                .expect_err("later edit must block post-commit narration adoption");
        assert!(
            error.starts_with("video.narration_result_missing:"),
            "{error}"
        );
        let unchanged = stale_runtime
            .video
            .get_project(&project_id)
            .expect("reload later narration edit");
        assert_eq!(unchanged["revision"], 3);
        assert_eq!(
            unchanged["manifest"]["narration_bindings"]
                .as_array()
                .expect("retained narration binding")
                .len(),
            1
        );
        assert_eq!(
            stale_runtime
                .store
                .job_status(&replacement_job_id)
                .expect("stale replacement status")
                .as_deref(),
            Some("failed")
        );

        drop(stale_runtime);
        drop(resumed);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn voice_route_requires_a_complete_compatible_consent_backed_selection() {
        let complete = UiScenePatch {
            layout: None,
            crop_mode: None,
            crop_rect: None,
            captions_enabled: None,
            caption_style: None,
            voice_gain_db: None,
            music_gain_db: None,
            voice_id: Some("af_heart".into()),
            model_id: Some("hexgrad/Kokoro-82M".into()),
            speaker: Some("af_heart".into()),
            language: Some("en-US".into()),
        };
        let selection = voice_revision_selection(Some(&complete))
            .expect("complete route")
            .expect("voice selection");
        validate_voice_model_compatibility(
            &selection,
            &json!({"engine":"kokoro"}),
            &json!({"state":"preset","consent":"not-required","engines":["Kokoro"]}),
        )
        .expect("compatible preset route");

        let mut partial = complete.clone();
        partial.language = None;
        assert!(voice_revision_selection(Some(&partial))
            .expect_err("reject partial route")
            .starts_with("video.voice_route_incomplete:"));

        let mut wrong_speaker = selection.clone();
        wrong_speaker.speaker = "default".into();
        assert!(validate_voice_model_compatibility(
            &wrong_speaker,
            &json!({"engine":"kokoro"}),
            &json!({"state":"preset","consent":"not-required","engines":["kokoro"]}),
        )
        .expect_err("reject preset speaker mismatch")
        .starts_with("video.voice_speaker_mismatch:"));

        let custom = VoiceRevisionSelection {
            voice_id: "custom-voice".into(),
            model_id: "local/chatterbox".into(),
            speaker: "default".into(),
            language: "en".into(),
        };
        assert!(validate_voice_model_compatibility(
            &custom,
            &json!({"engine":"chatterbox"}),
            &json!({"state":"ready","consent":"pending","engines":["chatterbox"]}),
        )
        .expect_err("reject missing confirmed consent")
        .starts_with("video.voice_consent_required:"));
    }

    #[test]
    fn durable_command_jobs_reject_cross_revision_resume() {
        let record = json!({
            "revision": 4,
            "version": { "id": "version-4" },
        });
        ensure_durable_project_expectation(&record, 4, "version-4").unwrap();
        assert!(ensure_durable_project_expectation(&record, 3, "version-3")
            .unwrap_err()
            .starts_with("video.resume_conflict:"));
        assert!(
            ensure_durable_project_expectation(&record, 4, "version-other")
                .unwrap_err()
                .starts_with("video.resume_conflict:")
        );
    }

    #[test]
    fn prompt_project_parent_is_atomic_discoverable_after_restart_and_reuses_children() {
        let root =
            std::env::temp_dir().join(format!("soundar-prompt-parent-recovery-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let store = crate::store::Store::open(data.clone(), artifacts.clone())
            .expect("open prompt recovery store");
        let request = DurablePromptVideoRequest {
            project_id: "reserved-prompt-project".into(),
            prompt: "Explain why local creative tools matter.".into(),
            prompt_sha256: sha256_text("Explain why local creative tools matter."),
            title: "Explain why local creative tools matter · Video draft".into(),
            actor: "test-suite".into(),
            model_id: "test/prompt-model".into(),
            speaker: "test-speaker".into(),
            voice_name: "Test voice".into(),
            language: "en".into(),
            priority: "normal".into(),
        };
        let durable = serde_json::to_value(&request).expect("encode durable prompt parent");
        let mut manifest = empty_project_manifest(&request.project_id, &request.title, 5_000_000)
            .expect("create initial prompt manifest");
        manifest.revision = 1;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(video::RevisionRecord {
            id: "initial-prompt-revision".into(),
            revision: 1,
            parent_id: None,
            actor: request.actor.clone(),
            reason: format!("Initial intent: {}", request.prompt),
            changed_paths: vec!["/".into()],
            invalidated_stages: BTreeSet::new(),
            created_at: manifest.updated_at.clone(),
        });
        manifest
            .validate_strict()
            .expect("strict initial prompt manifest");
        let manifest_value = serde_json::to_value(&manifest).expect("encode prompt manifest");
        let (created_project, parent_job_id) = store
            .create_video_project_with_job(
                &request.title,
                &manifest_value,
                &request.actor,
                "video_create_from_prompt",
                &durable,
                "video-create-prompt:reserved-prompt-project",
            )
            .expect("atomically create prompt project and parent");
        assert_eq!(created_project["id"], request.project_id);
        drop(store);

        // Opening the Store simulates application restart after the atomic transaction but before
        // the runner begins. The visible project resolves the exact failed parent for Resume.
        let reopened = crate::store::Store::open(data, artifacts).expect("restart prompt store");
        assert_eq!(
            reopened
                .job_status(&parent_job_id)
                .expect("read recovered parent")
                .as_deref(),
            Some("failed")
        );
        let recoverable = reopened
            .latest_video_project_job(&request.project_id)
            .expect("resolve restart recovery")
            .expect("recoverable prompt parent");
        assert_eq!(recoverable["id"], parent_job_id);
        let runtime =
            RuntimeState::new_with_store(root.join("runtime"), PathBuf::from("python3"), reopened);
        let raw_project = runtime
            .video
            .get_project(&request.project_id)
            .expect("load atomic prompt project through the shared service");
        let mut presented =
            video::present_video_project(&raw_project, &runtime.store.video_artifacts_root())
                .expect("present prompt project");
        attach_latest_project_job(&runtime, &request.project_id, &mut presented)
            .expect("attach exact recoverable prompt parent");
        assert_eq!(presented["workflow_job"]["id"], parent_job_id);
        assert_eq!(presented["workflow_job"]["phase"], "source");
        assert_eq!(presented["recoverable_job"]["id"], parent_job_id);
        let first = ensure_prompt_project(&runtime, &request).expect("adopt atomic draft");
        let second = ensure_prompt_project(&runtime, &request).expect("re-adopt atomic draft");
        assert_eq!(first["version"]["id"], second["version"]["id"]);
        assert_eq!(
            runtime
                .video
                .list_projects()
                .expect("list prompt projects")
                .len(),
            1
        );

        let synthesis_request = prompt_synthesis_request(&request, &parent_job_id);
        let first_child = runtime
            .store
            .create_idempotent_job(
                "synthesis",
                &format!("video-prompt-synthesis-{parent_job_id}"),
                &synthesis_request,
            )
            .expect("create prompt synthesis child")
            .expect("prompt synthesis identity");
        let replayed_child = runtime
            .store
            .create_idempotent_job(
                "synthesis",
                &format!("video-prompt-synthesis-{parent_job_id}"),
                &synthesis_request,
            )
            .expect("replay prompt synthesis child")
            .expect("matching synthesis identity");
        assert_eq!(first_child.0, replayed_child.0);
        assert!(first_child.1);
        assert!(!replayed_child.1);
        assert_eq!(synthesis_request["video_project_id"], request.project_id);
        assert_eq!(synthesis_request["video_purpose"], "prompt_narration");
        assert_eq!(synthesis_request["video_source_role"], "primary_narration");

        drop(runtime);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn prompt_parent_rearms_exact_import_after_post_manifest_restart_without_duplicates() {
        let root = std::env::temp_dir().join(format!(
            "soundar-prompt-post-manifest-restart-{}",
            Uuid::new_v4()
        ));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let store = crate::store::Store::open(data.clone(), artifacts.clone())
            .expect("open prompt post-manifest store");
        let request = DurablePromptVideoRequest {
            project_id: "prompt-post-manifest-project".into(),
            prompt: "A durable local-first creative workflow.".into(),
            prompt_sha256: sha256_text("A durable local-first creative workflow."),
            title: "Durable local workflow · Video draft".into(),
            actor: "test-suite".into(),
            model_id: "test/prompt-model".into(),
            speaker: "test-speaker".into(),
            voice_name: "Test voice".into(),
            language: "en".into(),
            priority: "normal".into(),
        };
        let durable = serde_json::to_value(&request).expect("encode prompt parent");
        let mut manifest = empty_project_manifest(&request.project_id, &request.title, 5_000_000)
            .expect("create prompt draft");
        manifest.revision = 1;
        manifest.updated_at = utc_now();
        manifest.revision_history.push(video::RevisionRecord {
            id: "prompt-initial-revision".into(),
            revision: 1,
            parent_id: None,
            actor: request.actor.clone(),
            reason: format!("Initial intent: {}", request.prompt),
            changed_paths: vec!["/".into()],
            invalidated_stages: BTreeSet::new(),
            created_at: manifest.updated_at.clone(),
        });
        manifest.validate_strict().expect("valid prompt draft");
        let (_, parent_job_id) = store
            .create_video_project_with_job(
                &request.title,
                &serde_json::to_value(&manifest).expect("encode prompt draft"),
                &request.actor,
                "video_create_from_prompt",
                &durable,
                "video-create-prompt:prompt-post-manifest-project",
            )
            .expect("atomically create prompt workflow");
        store
            .start_job(&parent_job_id)
            .expect("start prompt parent");

        let synthesis_request = prompt_synthesis_request(&request, &parent_job_id);
        let (synthesis_job_id, created) = store
            .create_idempotent_job(
                "synthesis",
                &format!("video-prompt-synthesis-{parent_job_id}"),
                &synthesis_request,
            )
            .expect("create exact prompt synthesis")
            .expect("prompt synthesis identity");
        assert!(created);
        store
            .start_job(&synthesis_job_id)
            .expect("start prompt synthesis");
        let audio_path = artifacts.join("prompt-post-manifest.wav");
        write_test_pcm_wav(&audio_path, 650);
        let history = store
            .complete_synthesis(
                &synthesis_job_id,
                &synthesis_request,
                &json!({
                    "id": "prompt-post-manifest-history",
                    "generation_kind": "speech",
                    "model_id": request.model_id,
                    "engine": "test-engine",
                    "audio_path": audio_path,
                    "sample_rate": 8_000,
                    "duration_seconds": 0.65,
                    "inference_seconds": 0.01,
                    "rtf": 0.02,
                    "vram_peak_mb": 0,
                    "waveform": [0.0],
                }),
            )
            .expect("publish exact prompt History");
        assert_eq!(history["id"], "prompt-post-manifest-history");

        let runtime = RuntimeState::new_with_store(
            root.join("runtime-first"),
            PathBuf::from("python3"),
            store,
        );
        let queued = runtime
            .video
            .queue_local_import_idempotent(
                prompt_local_import_request(&request, audio_path.clone()),
                &parent_job_id,
                None,
            )
            .expect("queue exact prompt import");
        let first = runtime
            .video
            .wait_for_job(&queued.job_id, &request.project_id, Duration::from_secs(60))
            .expect("finish prompt import through shared service")
            .project;
        validate_prompt_imported_project(
            &first,
            &request,
            "prompt-post-manifest-history",
            &synthesis_job_id,
        )
        .expect("exact first imported revision");
        let committed_revision = first["revision"].as_i64().expect("import revision");
        let committed_version = project_version_id(&first)
            .expect("import version")
            .to_string();
        runtime
            .store
            .simulate_worker_crash_after_commit(&queued.job_id)
            .expect("inject crash after prompt manifest commit");
        drop(runtime);
        let stale_data = root.join("stale-data");
        copy_test_directory(&data, &stale_data);

        let reopened = crate::store::Store::open(data.clone(), artifacts.clone())
            .expect("reopen after prompt worker crash");
        assert_eq!(
            reopened
                .job_status(&queued.job_id)
                .expect("read interrupted import")
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            reopened
                .job_status(&parent_job_id)
                .expect("read interrupted parent")
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            reopened
                .latest_video_project_job(&request.project_id)
                .expect("resolve prompt recovery owner")
                .expect("recoverable prompt parent")["id"],
            parent_job_id
        );
        reopened
            .resume_video_job(&parent_job_id, &["video_create_from_prompt"])
            .expect("rearm exact prompt parent");
        let resumed = RuntimeState::new_with_store(
            root.join("runtime-resumed"),
            PathBuf::from("python3"),
            reopened,
        );
        let recovered =
            run_prompt_video_job_guarded(&resumed, &parent_job_id, request.clone(), None)
                .expect("adopt post-manifest prompt import");
        assert_eq!(recovered["revision"], committed_revision);
        assert_eq!(
            project_version_id(&recovered).expect("recovered version"),
            committed_version
        );
        assert_eq!(
            recovered["manifest"]["source_assets"]
                .as_array()
                .expect("source assets")
                .len(),
            1
        );
        let assets = recovered["assets"].as_array().expect("registered assets");
        assert!(!assets.is_empty());
        assert!(assets
            .iter()
            .all(|asset| asset["status"].as_str() == Some("ready")));
        let asset_ids = assets
            .iter()
            .filter_map(|asset| asset["id"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(asset_ids.len(), assets.len());
        assert_eq!(
            resumed
                .store
                .video_child_job(&parent_job_id, "video_import_local")
                .expect("reload import child")
                .expect("bound import child"),
            (queued.job_id.clone(), "completed".into())
        );
        assert_eq!(
            resumed
                .store
                .job_status(&parent_job_id)
                .expect("read completed prompt parent")
                .as_deref(),
            Some("completed")
        );

        let mut later_edit = recovered.clone();
        later_edit["revision"] = json!(3);
        assert!(validate_prompt_imported_project(
            &later_edit,
            &request,
            "prompt-post-manifest-history",
            &synthesis_job_id,
        )
        .expect_err("never adopt a retained source from a later edit")
        .starts_with("video.prompt_import_mismatch:"));

        // Reopen the same post-commit crash snapshot, advance the project independently, and
        // prove the owning parent cannot adopt a retained source from that later revision.
        let stale_store = crate::store::Store::open(stale_data, artifacts.clone())
            .expect("open stale prompt crash snapshot");
        let stale_runtime = RuntimeState::new_with_store(
            root.join("runtime-stale"),
            PathBuf::from("python3"),
            stale_store,
        );
        let stale_current = stale_runtime
            .video
            .get_project(&request.project_id)
            .expect("load prompt result before unrelated edit");
        let mut stale_manifest: video::VideoProjectManifest =
            serde_json::from_value(stale_current["manifest"].clone())
                .expect("decode prompt result before unrelated edit");
        stale_manifest.name.push_str(" · later edit");
        advance_manifest_revision(
            &mut stale_manifest,
            "other-editor",
            "Rename after prompt import",
            vec!["/name".into()],
            BTreeSet::from([video::RevisionStage::PublishPackage]),
        )
        .expect("advance unrelated prompt edit");
        stale_runtime
            .video
            .revise_manifest(video::service::ReviseVideoManifestRequest {
                project_id: request.project_id.clone(),
                expected_revision: 2,
                manifest: stale_manifest,
                actor: "other-editor".into(),
                reason: "Rename after prompt import".into(),
                changed_paths: vec!["/name".into()],
                invalidated_stages: BTreeSet::from([video::RevisionStage::PublishPackage]),
                status: Some("ready".into()),
            })
            .expect("commit unrelated prompt edit");
        stale_runtime
            .store
            .resume_video_job(&parent_job_id, &["video_create_from_prompt"])
            .expect("rearm stale prompt parent");
        let error =
            run_prompt_video_job_guarded(&stale_runtime, &parent_job_id, request.clone(), None)
                .expect_err("later edit must block post-commit prompt adoption");
        assert!(error.starts_with("video.resume_conflict:"), "{error}");
        let unchanged = stale_runtime
            .video
            .get_project(&request.project_id)
            .expect("reload later prompt edit");
        assert_eq!(unchanged["revision"], 3);
        assert_eq!(
            unchanged["manifest"]["source_assets"]
                .as_array()
                .expect("retained prompt source")
                .len(),
            1
        );
        assert_eq!(
            stale_runtime
                .store
                .job_status(&queued.job_id)
                .expect("stale import status")
                .as_deref(),
            Some("failed")
        );

        drop(stale_runtime);
        drop(resumed);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn shortening_opening_updates_only_timeline_clock_dependents() {
        let mut manifest = revision_fixture();
        shorten_opening(&mut manifest).unwrap();
        assert_eq!(
            manifest.timeline_duration_us,
            video::Microseconds(3_600_000)
        );
        assert_eq!(
            manifest.reviewed_scenes[0].timeline_duration_us,
            video::Microseconds(1_600_000)
        );
        assert_eq!(
            manifest.reviewed_scenes[1].timeline_start_us,
            video::Microseconds(1_600_000)
        );
        assert_eq!(
            manifest.captions[1].range.start_us,
            video::Microseconds(1_600_000)
        );
        assert_eq!(
            manifest.tracks[0].clips[0].source_range.end_us,
            video::Microseconds(1_600_000)
        );
        manifest.validate_strict().unwrap();
    }

    #[test]
    fn structured_scene_patch_has_selective_diff_and_invalidation() {
        let mut before = revision_fixture();
        let mut overlay = before.tracks[0].clone();
        overlay.id = "overlay-main".into();
        overlay.kind = video::TrackKind::Overlay;
        for clip in &mut overlay.clips {
            clip.id = format!("overlay-{}", clip.id);
        }
        before.tracks.push(overlay);
        before.validate_strict().unwrap();
        let mut after = before.clone();
        let mut requested_paths = BTreeSet::new();
        let mut requested_stages = BTreeSet::new();
        apply_scene_patch(
            &mut after,
            Some("scene-opening"),
            &UiScenePatch {
                layout: Some("portrait".into()),
                crop_mode: Some("manual".into()),
                crop_rect: Some(video::NormalizedRect {
                    x_bp: 1_750,
                    y_bp: 500,
                    width_bp: 5_625,
                    height_bp: 9_000,
                }),
                captions_enabled: Some(true),
                caption_style: Some("calm".into()),
                voice_gain_db: Some(-3.0),
                music_gain_db: Some(-12.0),
                voice_id: None,
                model_id: None,
                speaker: None,
                language: None,
            },
            &mut requested_paths,
            &mut requested_stages,
        )
        .unwrap();
        let paths = manifest_diff_paths(&before, &after).unwrap();
        assert_eq!(
            paths,
            BTreeSet::from([
                "/audio_mix/tracks".to_string(),
                "/captions".to_string(),
                "/tracks".to_string(),
            ])
        );
        let invalidated = invalidation_for_manifest_changes(&paths);
        assert!(invalidated.contains(&video::RevisionStage::Captions));
        assert!(invalidated.contains(&video::RevisionStage::SceneRender));
        assert!(invalidated.contains(&video::RevisionStage::FinalRender));
        assert!(!invalidated.contains(&video::RevisionStage::Transcript));
        assert_eq!(after.audio_mix.tracks[0].gain_db_milli, -3_000);
        let expected_crop = video::NormalizedRect {
            x_bp: 1_750,
            y_bp: 500,
            width_bp: 5_625,
            height_bp: 9_000,
        };
        for track in &after.tracks {
            for clip in &track.clips {
                if clip.scene_id.as_deref() == Some("scene-opening")
                    && matches!(
                        track.kind,
                        video::TrackKind::Video | video::TrackKind::Overlay
                    )
                {
                    assert_eq!(clip.crop, Some(expected_crop));
                } else {
                    assert_eq!(clip.crop, None);
                }
            }
        }
        after.validate_strict().unwrap();

        let mut missing = revision_fixture();
        assert!(
            set_scene_crop(&mut missing, "scene-opening", "manual", None)
                .expect_err("manual framing needs exact normalized coordinates")
                .starts_with("video.invalid_crop:")
        );
    }

    #[test]
    fn native_caption_style_mapping_matches_the_shared_curated_contract() {
        for preset in video::CaptionPresetId::ALL {
            assert_eq!(
                caption_style_id(preset.public_id()).unwrap(),
                preset.manifest_id()
            );
            assert_eq!(
                caption_style_id(preset.manifest_id()).unwrap(),
                preset.manifest_id()
            );
        }
        let error = caption_style_id("unknown-style").unwrap_err();
        assert!(error.starts_with("video.invalid_caption_style:"));
        for style in video::CaptionPresetId::PUBLIC_IDS {
            assert!(error.contains(style));
        }
    }

    #[test]
    fn narration_binding_changes_invalidate_speech_and_downstream_renders() {
        let before = revision_fixture();
        let mut after = before.clone();
        after.narration_bindings.push(video::NarrationBinding {
            id: "binding-opening".into(),
            scene_id: Some("scene-opening".into()),
            render_artifact_id: "speech-opening".into(),
            history_id: "history-opening".into(),
            generation_job_id: "synthesis-opening".into(),
            voice_id: "af_heart".into(),
            model_id: "hexgrad/Kokoro-82M".into(),
            speaker: "af_heart".into(),
            language: "en-US".into(),
            script_sha256: sha256_text("A concise opening statement."),
            created_at: "2026-08-27T20:00:00.000Z".into(),
        });
        let paths = manifest_diff_paths(&before, &after).unwrap();
        assert_eq!(paths, BTreeSet::from(["/narration_bindings".to_string()]));
        let invalidated = invalidation_for_manifest_changes(&paths);
        assert!(invalidated.contains(&video::RevisionStage::Speech));
        assert!(invalidated.contains(&video::RevisionStage::SceneRender));
        assert!(invalidated.contains(&video::RevisionStage::Preview));
        assert!(invalidated.contains(&video::RevisionStage::FinalRender));
        assert!(invalidated.contains(&video::RevisionStage::PublishPackage));
        assert!(!invalidated.contains(&video::RevisionStage::Transcript));
    }
}
