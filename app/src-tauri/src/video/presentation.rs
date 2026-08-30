//! Transport presentation for the native Video Studio surface.
//!
//! Persistence and rendering use the strict canonical manifest. The React workspace needs a
//! compact, media-oriented projection with millisecond display clocks and directly playable
//! artifact paths. Keeping this adapter pure prevents the UI, Tauri commands, and agent tools
//! from growing separate workflow implementations.

use super::assembly::CaptionPreviewPage;
use super::contracts::{
    caption_bounds_for_scene, CandidateStatus, CanvasMode, CaptionPresetId, LayoutRole,
    Microseconds, PublicationState, RenderArtifact, RenderArtifactRole, RevisionStage, SourceAsset,
    SourceAssetKind, TrackKind, VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
};
use super::media::{MediaRuntimeStatus, MediaToolStatus};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub fn present_video_project(record: &Value, video_root: &Path) -> VideoResult<Value> {
    let manifest_value = record
        .get("manifest")
        .cloned()
        .ok_or_else(|| presentation_error("the stored video project is missing its manifest"))?;
    let manifest: VideoProjectManifest =
        serde_json::from_value(manifest_value).map_err(|error| {
            presentation_error(format!(
                "the stored video manifest could not be decoded: {error}"
            ))
        })?;
    manifest.validate_strict()?;

    let project_id = required_string(record, "id")?;
    if manifest.project_id != project_id {
        return Err(presentation_error(
            "the stored project and manifest identifiers do not match",
        ));
    }
    let version_id = record
        .pointer("/version/id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let store_assets = record
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let outputs = record
        .get("outputs")
        .and_then(Value::as_array)
        .map(|outputs| {
            outputs
                .iter()
                .filter(|output| {
                    output.get("version_id").and_then(Value::as_str) == Some(version_id.as_str())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let local_by_id = store_assets
        .iter()
        .filter_map(|asset| {
            Some((
                asset.get("id")?.as_str()?.to_string(),
                asset.get("local_path")?.as_str()?.to_string(),
            ))
        })
        .collect::<BTreeMap<_, _>>();

    let deliverables = outputs
        .iter()
        .map(|output| present_output(output, &project_id, &version_id))
        .collect::<VideoResult<Vec<_>>>()?;
    let mut artifacts = Vec::new();
    let mut artifact_ids = BTreeSet::new();
    for artifact in &deliverables {
        if let Some(id) = artifact.get("id").and_then(Value::as_str) {
            artifact_ids.insert(id.to_string());
        }
        artifacts.push(artifact.clone());
    }
    for artifact in &manifest.render_artifacts {
        if artifact_ids.insert(artifact.id.clone()) {
            artifacts.push(present_manifest_artifact(
                artifact,
                &project_id,
                &version_id,
                video_root,
            ));
        }
    }
    for asset in &store_assets {
        if !matches!(
            asset.get("kind").and_then(Value::as_str),
            Some("proxy" | "thumbnail" | "waveform")
        ) {
            continue;
        }
        let Some(id) = asset.get("id").and_then(Value::as_str) else {
            continue;
        };
        if artifact_ids.insert(id.to_string()) {
            artifacts.push(present_media_asset(asset, &project_id, &version_id));
        }
    }

    let master = deliverables
        .iter()
        .find(|artifact| artifact.get("role").and_then(Value::as_str) == Some("master"))
        .cloned();
    let source = present_source(
        manifest.source_assets.first(),
        &manifest,
        &local_by_id,
        &artifacts,
        video_root,
    );
    let transcript_segments = manifest
        .transcript
        .as_ref()
        .map(|transcript| {
            transcript
                .segments
                .iter()
                .map(|segment| {
                    json!({
                        "id": segment.id,
                        "start_ms": micros_to_millis(segment.range.start_us),
                        "end_ms": micros_to_millis(segment.range.end_us),
                        "text": segment.text,
                        "speaker": segment.speaker_id,
                        "source_clock": true,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let segment_text = manifest
        .transcript
        .as_ref()
        .map(|transcript| {
            transcript
                .segments
                .iter()
                .map(|segment| (segment.id.as_str(), segment.text.as_str()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let candidates = manifest
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            json!({
                "id": candidate.id,
                "rank": index + 1,
                "source_start_ms": micros_to_millis(candidate.source_range.start_us),
                "source_end_ms": micros_to_millis(candidate.source_range.end_us),
                "title": candidate.title,
                "transcript": candidate.transcript_segment_ids.iter().filter_map(|id| segment_text.get(id.as_str()).copied()).collect::<Vec<_>>().join(" "),
                "score": f64::from(candidate.score_milli) / 10.0,
                "selected": matches!(candidate.status, CandidateStatus::Accepted),
                "poster_url": Value::Null,
            })
        })
        .collect::<Vec<_>>();

    let caption_style = current_caption_style(&manifest);
    let caption_pages = super::assembly::plan_caption_preview_pages(&manifest)?;
    let presented_caption_pages = caption_pages
        .iter()
        .map(present_caption_page)
        .collect::<Vec<_>>();
    let layout = canvas_mode(&manifest);
    let narration_by_scene = manifest
        .narration_bindings
        .iter()
        .filter_map(|binding| Some((binding.scene_id.as_deref()?, binding)))
        .collect::<BTreeMap<_, _>>();
    let caption_bounds_by_scene = manifest
        .reviewed_scenes
        .iter()
        .map(|scene| {
            caption_bounds_for_scene(&manifest.layout, Some(&scene.id))
                .map(|bounds| (scene.id.as_str(), bounds))
        })
        .collect::<VideoResult<BTreeMap<_, _>>>()?;
    let scenes = manifest
        .reviewed_scenes
        .iter()
        .enumerate()
        .map(|(index, scene)| {
            let narration = narration_by_scene.get(scene.id.as_str()).copied();
            let scene_caption_style = caption_style_for_scene(&manifest, &scene.id)
                .unwrap_or(caption_style);
            let source_range = scene.source_range;
            let timeline_end = scene
                .timeline_start_us
                .0
                .saturating_add(scene.timeline_duration_us.0);
            json!({
                "id": scene.id,
                "candidate_id": scene.candidate_id,
                "position": index + 1,
                "title": scene.title,
                "source_start_ms": source_range.map(|range| micros_to_millis(range.start_us)).unwrap_or(0),
                "source_end_ms": source_range.map(|range| micros_to_millis(range.end_us)).unwrap_or_else(|| micros_to_millis(scene.timeline_duration_us)),
                "timeline_start_ms": micros_to_millis(scene.timeline_start_us),
                "timeline_end_ms": micros_to_millis(Microseconds(timeline_end)),
                "transcript": scene.script,
                "layout": layout,
                "crop_mode": scene_crop_mode(&manifest, &scene.id),
                "crop_rect": scene_crop_rect(&manifest, &scene.id),
                "captions_enabled": manifest.captions.iter().any(|caption| caption.scene_id.as_deref() == Some(scene.id.as_str())),
                "caption_style": scene_caption_style,
                "caption_bounds": caption_bounds_by_scene.get(scene.id.as_str()),
                "voice_gain_db": gain_for_track(&manifest, "audio-main"),
                "music_gain_db": gain_for_track(&manifest, "music-main"),
                "narration_binding_id": narration.map(|binding| binding.id.as_str()),
                "narration_history_id": narration.map(|binding| binding.history_id.as_str()),
                "voice_id": narration.map(|binding| binding.voice_id.as_str()),
                "model_id": narration.map(|binding| binding.model_id.as_str()),
                "speaker": narration.map(|binding| binding.speaker.as_str()),
                "language": narration.map(|binding| binding.language.as_str()),
            })
        })
        .collect::<Vec<_>>();
    let narration_bindings = manifest
        .narration_bindings
        .iter()
        .map(|binding| {
            json!({
                "id": binding.id,
                "scene_id": binding.scene_id,
                "render_artifact_id": binding.render_artifact_id,
                "history_id": binding.history_id,
                "generation_job_id": binding.generation_job_id,
                "voice_id": binding.voice_id,
                "model_id": binding.model_id,
                "speaker": binding.speaker,
                "language": binding.language,
                "script_sha256": binding.script_sha256,
                "created_at": binding.created_at,
                "turn_id": binding.turn_id,
            })
        })
        .collect::<Vec<_>>();
    // The cast and the script are presented together: a turn is only readable next to the
    // character who speaks it, and the UI needs the voice route to explain a take.
    let cast = manifest
        .cast
        .iter()
        .map(|member| {
            json!({
                "id": member.id,
                "name": member.name,
                "display_name": member.display_name,
                "voice_id": member.voice_id,
                "model_id": member.model_id,
                "language": member.language,
                "delivery": member.delivery,
                "consent_reference_id": member.consent_reference_id,
                "notes": member.notes,
                "created_at": member.created_at,
            })
        })
        .collect::<Vec<_>>();
    let narrated_turn_ids = manifest
        .narration_bindings
        .iter()
        .filter_map(|binding| binding.turn_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    let dialogue = manifest
        .dialogue
        .iter()
        .map(|turn| {
            json!({
                "id": turn.id,
                "scene_id": turn.scene_id,
                "order": turn.order,
                "character_id": turn.character_id,
                "text": turn.text,
                "direction": turn.direction,
                "source_line": turn.source_line,
                "revision": turn.revision,
                // Whether this line has a valid take is the one thing a reader most needs and
                // cannot derive without cross-referencing the bindings themselves.
                "narrated": narrated_turn_ids.contains(turn.id.as_str()),
            })
        })
        .collect::<Vec<_>>();
    // Rules are presented with the project so a reader can see why a name is spoken the way it is
    // without opening the manifest.
    let lexicon = manifest
        .lexicon
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "scope": entry.scope,
                "character_id": entry.character_id,
                "match_text": entry.match_text,
                "replacement": entry.replacement,
                "matching": entry.matching,
                "notes": entry.notes,
                "created_at": entry.created_at,
            })
        })
        .collect::<Vec<_>>();
    // The managed source carries the media facts, so they are resolved rather than duplicated.
    let source_by_id = manifest
        .source_assets
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let sound_assets = manifest
        .sound_assets
        .iter()
        .map(|asset| {
            let source = source_by_id.get(asset.source_asset_id.as_str());
            json!({
                "id": asset.id,
                "name": asset.name,
                "source_asset_id": asset.source_asset_id,
                "local_path": source.map(|source| managed_path(video_root, &source.managed_path)),
                "duration_ms": source.map(|source| micros_to_millis(source.probe.duration_us)),
                "tags": asset.tags,
                "created_at": asset.created_at,
            })
        })
        .collect::<Vec<_>>();
    let sound_layers = manifest
        .sound_layers
        .iter()
        .map(|layer| {
            json!({
                "id": layer.id,
                "asset_id": layer.asset_id,
                "kind": layer.kind,
                "scene_id": layer.scene_id,
                "turn_id": layer.turn_id,
                "start_ms": micros_to_millis(layer.range.start_us),
                "end_ms": micros_to_millis(layer.range.end_us),
                "gain_db": f64::from(layer.gain_db_milli) / 1000.0,
                "fade_in_ms": micros_to_millis(layer.fade_in_us),
                "fade_out_ms": micros_to_millis(layer.fade_out_us),
                "loop_to_fill": layer.loop_to_fill,
                "duck_under_speech": layer.duck_under_speech,
            })
        })
        .collect::<Vec<_>>();
    // A cue reports the length it was asked for. The fitted length is only real once local
    // generation has produced audio, so a planned cue reports no duration of its own.
    let music_cues = manifest
        .music_cues
        .iter()
        .map(|cue| {
            json!({
                "id": cue.id,
                "role": cue.role,
                "anchor": cue.anchor,
                "target_duration_ms": micros_to_millis(cue.target_duration_us),
                "direction": cue.direction,
                "source_asset_id": cue.source_asset_id,
                "track_id": cue.track_id,
                "gain_db": f64::from(cue.gain_db_milli) / 1000.0,
                "fade_in_ms": micros_to_millis(cue.fade_in_us),
                "fade_out_ms": micros_to_millis(cue.fade_out_us),
                "needs_generation": cue.needs_generation(),
                "created_at": cue.created_at,
            })
        })
        .collect::<Vec<_>>();
    // Beats are presented beside the dialogue because a pause is only meaningful next to the
    // lines it separates, and the UI must be able to show which ones the writer chose.
    let turn_beats = manifest
        .turn_beats
        .iter()
        .map(|beat| {
            json!({
                "turn_id": beat.turn_id,
                "lead_in_ms": micros_to_millis(beat.lead_in_us),
                "overlap_ms": micros_to_millis(beat.overlap_us),
                "source": beat.source,
            })
        })
        .collect::<Vec<_>>();
    let visual_assets = manifest
        .visual_assets
        .iter()
        .map(|asset| {
            json!({
                "id": asset.id,
                "mime_type": asset.mime_type.as_mime(),
                "local_path": managed_path(video_root, &asset.managed_path),
                "width": asset.width,
                "height": asset.height,
                "has_alpha": asset.has_alpha,
                "size_bytes": asset.size_bytes,
                "checksum": asset.sha256,
                "provenance": asset.provenance,
                "created_at": asset.created_at,
            })
        })
        .collect::<Vec<_>>();
    let visual_layers = manifest
        .visual_layers
        .iter()
        .map(|layer| {
            json!({
                "id": layer.id,
                "asset_id": layer.asset_id,
                "scene_id": layer.scene_id,
                "start_ms": micros_to_millis(layer.range.start_us),
                "end_ms": micros_to_millis(layer.range.end_us),
                "fit": layer.fit,
                "crop": layer.crop,
                "z_index": layer.z_index,
                "motion": layer.motion,
                "transition_in_ms": micros_to_millis(layer.transition_in_us),
                "transition_out_ms": micros_to_millis(layer.transition_out_us),
            })
        })
        .collect::<Vec<_>>();
    let timeline = present_timeline(&manifest, &caption_pages);
    let revisions = present_revisions(&manifest, &version_id);
    let status = present_status(record.get("status").and_then(Value::as_str));
    let name = record
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(manifest.name.as_str());
    let created_at = record
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or(manifest.created_at.as_str());
    let updated_at = record
        .get("updated_at")
        .and_then(Value::as_str)
        .unwrap_or(manifest.updated_at.as_str());
    let duration_ms = micros_to_millis(manifest.timeline_duration_us);
    // A generated still is the most faithful idle preview for illustrated/audio-led projects.
    // Imported video projects fall back to their derived source thumbnail; waveforms are never
    // promoted to a video poster merely because they are images.
    let poster_url = visual_assets
        .first()
        .and_then(|asset| asset.get("local_path"))
        .cloned()
        .or_else(|| {
            artifacts
                .iter()
                .find(|artifact| {
                    artifact.get("format").and_then(Value::as_str) == Some("image")
                        && artifact
                            .get("title")
                            .and_then(Value::as_str)
                            .is_some_and(|title| title.to_ascii_lowercase().contains("thumbnail"))
                })
                .and_then(|artifact| artifact.get("local_path"))
                .cloned()
        })
        .unwrap_or(Value::Null);

    Ok(json!({
        "id": project_id,
        "name": name,
        "status": status,
        "revision": record.get("revision").and_then(Value::as_i64).unwrap_or_else(|| i64::try_from(manifest.revision).unwrap_or(i64::MAX)),
        "duration_ms": duration_ms,
        "scene_count": manifest.reviewed_scenes.len(),
        "updated_at": updated_at,
        "poster_url": poster_url,
        "master": master,
        "deliverables": deliverables,
        "created_at": created_at,
        "manifest": {
            "schema_version": 1,
            "version_id": version_id,
            "source": source,
            "transcript_version": manifest.transcript.as_ref().map(|transcript| transcript.id.as_str()).unwrap_or(""),
            "transcript": transcript_segments,
            "candidates": candidates,
            "scenes": scenes,
            "caption_pages": presented_caption_pages,
            "narration_bindings": narration_bindings,
            "cast": cast,
            "dialogue": dialogue,
            "lexicon": lexicon,
            "music_cues": music_cues,
            "sound_assets": sound_assets,
            "sound_layers": sound_layers,
            "turn_beats": turn_beats,
            "performance_clock": {
                "intra_exchange_ms": micros_to_millis(manifest.performance_clock.intra_exchange_us),
                "turn_of_thought_ms": micros_to_millis(manifest.performance_clock.turn_of_thought_us),
                "pre_reveal_ms": micros_to_millis(manifest.performance_clock.pre_reveal_us),
                "scene_boundary_ms": micros_to_millis(manifest.performance_clock.scene_boundary_us),
            },
            "visual_assets": visual_assets,
            "visual_layers": visual_layers,
            "timeline": timeline,
            "artifacts": artifacts,
            "revisions": revisions,
            "settings": {
                "aspect_ratio": aspect_ratio(&manifest),
                "caption_style": caption_style,
                "captions_enabled": !manifest.captions.is_empty(),
                "hardware_render": true,
            },
        },
    }))
}

pub fn present_video_project_summary(record: &Value, video_root: &Path) -> VideoResult<Value> {
    let mut presented = present_video_project(record, video_root)?;
    let object = presented
        .as_object_mut()
        .ok_or_else(|| presentation_error("the project projection was not an object"))?;
    object.remove("manifest");
    object.remove("created_at");
    Ok(presented)
}

pub fn present_runtime_tools(
    status: &MediaRuntimeStatus,
    bundled_whisper_ready: bool,
) -> Vec<Value> {
    let javascript = if status.deno.available {
        (&status.deno, "Deno")
    } else {
        (&status.node, "Node")
    };
    let transcriber = if status.faster_whisper.available {
        (&status.faster_whisper, "faster-whisper")
    } else if status.whisper_cpp.available {
        (&status.whisper_cpp, "whisper.cpp")
    } else {
        (&status.faster_whisper, "soundAr Whisper")
    };
    vec![
        present_tool("ffmpeg", "FFmpeg", &status.ffmpeg, false),
        present_tool("ffprobe", "FFprobe", &status.ffprobe, false),
        present_tool("yt-dlp", "yt-dlp", &status.yt_dlp, false),
        present_tool("javascript", javascript.1, javascript.0, false),
        present_tool(
            "transcriber",
            transcriber.1,
            transcriber.0,
            bundled_whisper_ready,
        ),
    ]
}

fn present_source(
    source: Option<&SourceAsset>,
    manifest: &VideoProjectManifest,
    local_by_id: &BTreeMap<String, String>,
    artifacts: &[Value],
    video_root: &Path,
) -> Value {
    let Some(source) = source else {
        return json!({
            "id": format!("prompt-{}", manifest.project_id),
            "kind": "prompt",
            "display_name": manifest.name,
            "duration_ms": micros_to_millis(manifest.timeline_duration_us),
            "rights_confirmed": true,
            "provenance": "Created locally from a soundAr prompt",
        });
    };
    let local_path = local_by_id.get(&source.id).cloned().unwrap_or_else(|| {
        managed_path(video_root, &source.managed_path)
            .to_string_lossy()
            .to_string()
    });
    let poster = artifacts
        .iter()
        .find(|artifact| artifact.get("format").and_then(Value::as_str) == Some("image"))
        .and_then(|artifact| artifact.get("local_path"))
        .cloned()
        .unwrap_or(Value::Null);
    let display_name = Path::new(&local_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source.id.as_str());
    json!({
        "id": source.id,
        "kind": match source.kind {
            SourceAssetKind::ImportedLink => "link",
            SourceAssetKind::LocalVideo => "local-video",
            SourceAssetKind::LocalAudio | SourceAssetKind::SoundArSpeech | SourceAssetKind::SoundArMusic | SourceAssetKind::SoundArProject => "audio",
            SourceAssetKind::Generated => "prompt",
        },
        "exact_url": source.provenance.original_uri,
        "local_path": local_path,
        "display_name": display_name,
        "duration_ms": micros_to_millis(source.probe.duration_us),
        "width": source.probe.width,
        "height": source.probe.height,
        "mime_type": source_mime(source),
        "rights_confirmed": !matches!(source.kind, SourceAssetKind::ImportedLink) || source.rights_confirmation_id.is_some(),
        "rights_confirmation_url": source.provenance.original_uri,
        "poster_url": poster,
        "provenance": format!("{} · {}", source.provenance.producer, provenance_label(source)),
    })
}

fn present_output(
    output: &Value,
    project_id: &str,
    default_version_id: &str,
) -> VideoResult<Value> {
    let id = required_string(output, "id")?;
    let path = required_string(output, "artifact_path")?;
    let mime = output
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let kind = output
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("output");
    let primary = output
        .get("is_primary")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let role = if primary {
        "master"
    } else if kind.contains("package") || mime == "application/zip" {
        "publish-package"
    } else if kind.contains("preview") || kind.contains("proxy") {
        "preview"
    } else {
        "variation"
    };
    Ok(json!({
        "id": id,
        "project_id": project_id,
        "version_id": output.get("version_id").and_then(Value::as_str).unwrap_or(default_version_id),
        "role": role,
        "title": output.get("label").and_then(Value::as_str).unwrap_or("Video output"),
        "mime_type": mime,
        "format": format_label(mime, &path),
        "local_path": path,
        "download_name": Path::new(&path).file_name().and_then(|name| name.to_str()),
        "duration_ms": output.get("duration_us").and_then(Value::as_i64).map(|value| value / 1_000),
        "width": output.get("width"),
        "height": output.get("height"),
        "frame_rate": 30,
        "codec": if mime == "video/mp4" { Value::String("H.264".into()) } else { Value::Null },
        "file_size_bytes": output.get("size_bytes"),
        "checksum": output.get("sha256"),
        "playable": mime.starts_with("video/") || mime.starts_with("audio/"),
        "created_at": output.get("created_at").and_then(Value::as_str).unwrap_or("1970-01-01T00:00:00Z"),
    }))
}

/// Project an already-persisted output through the same playable/downloadable contract used by
/// full project presentation. Command and assistant adapters use this for publish-package results
/// without exposing a raw managed filesystem path as their final answer.
pub fn present_video_output(
    output: &Value,
    project_id: &str,
    default_version_id: &str,
) -> VideoResult<Value> {
    present_output(output, project_id, default_version_id)
}

fn present_manifest_artifact(
    artifact: &RenderArtifact,
    project_id: &str,
    version_id: &str,
    video_root: &Path,
) -> Value {
    let role = match artifact.role {
        RenderArtifactRole::Proxy => "proxy",
        RenderArtifactRole::Preview => "preview",
        RenderArtifactRole::FinalMaster => "master",
        RenderArtifactRole::PublishPackage => "publish-package",
        RenderArtifactRole::Thumbnail
        | RenderArtifactRole::Waveform
        | RenderArtifactRole::SceneSegment
        | RenderArtifactRole::Captions
        | RenderArtifactRole::Transcript => "variation",
    };
    let path = managed_path(video_root, &artifact.managed_path);
    json!({
        "id": artifact.id,
        "project_id": project_id,
        "version_id": version_id,
        "role": role,
        "title": artifact_title(artifact),
        "mime_type": artifact.mime_type,
        "format": format_label(&artifact.mime_type, &artifact.managed_path),
        "local_path": path,
        "download_name": path.file_name().and_then(|name| name.to_str()),
        "duration_ms": artifact.duration_us.map(micros_to_millis),
        "width": artifact.width,
        "height": artifact.height,
        "frame_rate": 30,
        "codec": if artifact.mime_type == "video/mp4" { Value::String("H.264".into()) } else { Value::Null },
        "checksum": artifact.sha256,
        "playable": matches!(artifact.publication_state, PublicationState::Published) && (artifact.mime_type.starts_with("video/") || artifact.mime_type.starts_with("audio/")),
        "created_at": artifact.created_at,
    })
}

fn present_media_asset(asset: &Value, project_id: &str, version_id: &str) -> Value {
    let path = asset
        .get("local_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mime = asset
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let kind = asset.get("kind").and_then(Value::as_str).unwrap_or("proxy");
    json!({
        "id": asset.get("id"),
        "project_id": project_id,
        "version_id": version_id,
        "role": if kind == "proxy" { "proxy" } else { "variation" },
        "title": match kind { "thumbnail" => "Source thumbnail", "waveform" => "Audio waveform", _ => "Source proxy" },
        "mime_type": mime,
        "format": if kind == "thumbnail" || kind == "waveform" { "image" } else { format_label(mime, path) },
        "local_path": path,
        "download_name": Path::new(path).file_name().and_then(|name| name.to_str()),
        "duration_ms": asset.get("duration_us").and_then(Value::as_i64).map(|value| value / 1_000),
        "width": asset.pointer("/probe/width"),
        "height": asset.pointer("/probe/height"),
        "file_size_bytes": asset.get("size_bytes"),
        "checksum": asset.get("content_sha256"),
        "playable": mime.starts_with("video/") || mime.starts_with("audio/"),
        "created_at": asset.get("created_at").and_then(Value::as_str).unwrap_or("1970-01-01T00:00:00Z"),
    })
}

fn present_timeline(
    manifest: &VideoProjectManifest,
    caption_pages: &[CaptionPreviewPage],
) -> Value {
    let source_kind = manifest
        .source_assets
        .iter()
        .map(|source| (source.id.as_str(), &source.kind))
        .collect::<BTreeMap<_, _>>();
    let mut tracks = Vec::new();
    for track in &manifest.tracks {
        // Caption media tracks are an internal timeline contract. The authoritative preview lane
        // is derived from the same paged cue plan as ASS below, so never expose a competing lane
        // that could shadow it in clients using `find(kind == "captions")`.
        if matches!(track.kind, TrackKind::Caption) {
            continue;
        }
        let frontend_kind = match track.kind {
            TrackKind::Video | TrackKind::Overlay => "video",
            TrackKind::Caption => unreachable!("caption tracks are projected from cue pages"),
            TrackKind::Audio => {
                let music = track.clips.iter().any(|clip| {
                    clip.media
                        .source_asset_id
                        .as_deref()
                        .and_then(|id| source_kind.get(id))
                        .is_some_and(|kind| matches!(kind, SourceAssetKind::SoundArMusic))
                });
                if music {
                    "music"
                } else {
                    "voice"
                }
            }
        };
        let mut items = track
            .clips
            .iter()
            .map(|clip| {
                json!({
                    "id": clip.id,
                    "track": frontend_kind,
                    "kind": "clip",
                    "start_ms": micros_to_millis(clip.timeline_start_us),
                    "end_ms": micros_to_millis(Microseconds(clip.timeline_start_us.0.saturating_add(clip.timeline_duration_us.0))),
                    "label": clip.scene_id.as_deref().unwrap_or("Media clip"),
                    "scene_id": clip.scene_id,
                    "source_start_ms": micros_to_millis(clip.source_range.start_us),
                    "source_end_ms": micros_to_millis(clip.source_range.end_us),
                })
            })
            .collect::<Vec<_>>();
        items.extend(
            manifest
                .gaps
                .iter()
                .filter(|gap| gap.track_id == track.id)
                .map(|gap| {
                    json!({
                        "id": gap.id,
                        "track": frontend_kind,
                        "kind": "gap",
                        "start_ms": micros_to_millis(gap.range.start_us),
                        "end_ms": micros_to_millis(gap.range.end_us),
                        "label": match gap.reason {
                            super::contracts::GapReason::SourceSilence => "Preserved source silence",
                            super::contracts::GapReason::Editorial => "Editorial pause",
                            super::contracts::GapReason::Transition => "Transition",
                            super::contracts::GapReason::Padding => "Padding",
                        },
                    })
                }),
        );
        items.sort_by_key(|item| item.get("start_ms").and_then(Value::as_i64).unwrap_or(0));
        tracks.push(json!({"kind": frontend_kind, "items": items}));
    }
    if !caption_pages.is_empty() {
        tracks.push(json!({
            "kind": "captions",
            "items": caption_pages.iter().map(|page| json!({
                "id": page.id,
                "cue_id": page.cue_id,
                "track": "captions",
                "kind": "clip",
                "start_ms": micros_to_millis(page.start_us),
                "end_ms": micros_to_millis(page.end_us),
                "label": page.text,
                "scene_id": page.scene_id,
                "caption_style": page.style_id,
                "bounds": page.bounds,
                "font_size_bp": page.font_size_bp,
            })).collect::<Vec<_>>(),
        }));
    }
    if !manifest.visual_layers.is_empty() {
        let title_by_asset = manifest
            .visual_assets
            .iter()
            .map(|asset| (asset.id.as_str(), asset.provenance.producer.as_str()))
            .collect::<BTreeMap<_, _>>();
        tracks.push(json!({
            "kind": "visuals",
            "items": manifest.visual_layers.iter().map(|layer| json!({
                "id": layer.id,
                "track": "visuals",
                "kind": "clip",
                "start_ms": micros_to_millis(layer.range.start_us),
                "end_ms": micros_to_millis(layer.range.end_us),
                "label": title_by_asset.get(layer.asset_id.as_str()).copied().unwrap_or("Visual"),
                "scene_id": layer.scene_id,
                "asset_id": layer.asset_id,
                "start_bounds": layer.motion.start_bounds,
                "end_bounds": layer.motion.end_bounds,
                "fit": layer.fit,
                "z_index": layer.z_index,
            })).collect::<Vec<_>>(),
        }));
    }
    let source_duration = manifest
        .source_assets
        .iter()
        .map(|source| source.probe.duration_us)
        .max()
        .unwrap_or(manifest.timeline_duration_us);
    json!({
        "duration_ms": micros_to_millis(manifest.timeline_duration_us),
        "source_clock_duration_ms": micros_to_millis(source_duration),
        "tracks": tracks,
    })
}

fn present_caption_page(page: &CaptionPreviewPage) -> Value {
    json!({
        "id": page.id,
        "cue_id": page.cue_id,
        "scene_id": page.scene_id,
        "start_ms": micros_to_millis(page.start_us),
        "end_ms": micros_to_millis(page.end_us),
        "text": page.text,
        "style_id": page.style_id,
        "bounds": page.bounds,
        "font_size_bp": page.font_size_bp,
        "words": page.words.iter().map(|word| json!({
            "text": word.text,
            "start_ms": micros_to_millis(word.start_us),
            "end_ms": micros_to_millis(word.end_us),
        })).collect::<Vec<_>>(),
    })
}

fn present_revisions(manifest: &VideoProjectManifest, current_version_id: &str) -> Vec<Value> {
    manifest
        .revision_history
        .iter()
        .enumerate()
        .map(|(index, revision)| {
            let affected = revision
                .invalidated_stages
                .iter()
                .map(revision_phase)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            json!({
                "id": revision.id,
                "created_at": revision.created_at,
                "instruction": revision.reason,
                "affected_stages": affected,
                "base_version_id": revision.parent_id.as_deref().unwrap_or(""),
                "version_id": if index + 1 == manifest.revision_history.len() { current_version_id.to_string() } else { format!("revision-{}", revision.revision) },
            })
        })
        .collect()
}

fn present_tool(id: &str, label: &str, tool: &MediaToolStatus, fallback_ready: bool) -> Value {
    let available = tool.available || fallback_ready;
    let detail = if fallback_ready && !tool.available {
        Some("Bundled soundAr Whisper runtime".to_string())
    } else {
        tool.version
            .clone()
            .or_else(|| tool.diagnostic.clone())
            .or_else(|| tool.setup_action.clone())
    };
    json!({
        "id": id,
        "label": label,
        "state": if available { "ready" } else if tool.setup_action.is_some() { "setup-needed" } else { "unavailable" },
        "detail": detail,
    })
}

fn current_caption_style(manifest: &VideoProjectManifest) -> &'static str {
    manifest
        .captions
        .first()
        .map(caption_public_style)
        .unwrap_or(CaptionPresetId::CleanWhite.public_id())
}

fn caption_style_for_scene(
    manifest: &VideoProjectManifest,
    scene_id: &str,
) -> Option<&'static str> {
    manifest
        .captions
        .iter()
        .find(|caption| caption.scene_id.as_deref() == Some(scene_id))
        .map(caption_public_style)
}

fn caption_public_style(caption: &super::contracts::CaptionCue) -> &'static str {
    // The manifest has already passed strict validation before presentation.
    CaptionPresetId::parse(&caption.style_id)
        .expect("validated caption preset")
        .public_id()
}

fn scene_crop_mode(manifest: &VideoProjectManifest, scene_id: &str) -> &'static str {
    if manifest
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .any(|clip| clip.scene_id.as_deref() == Some(scene_id) && clip.crop.is_some())
    {
        "manual"
    } else if manifest.layout.elements.iter().any(|element| {
        element.scene_id.as_deref() == Some(scene_id)
            && matches!(element.role, LayoutRole::PrimaryVideo)
    }) {
        "auto-center"
    } else {
        "fit"
    }
}

fn scene_crop_rect(
    manifest: &VideoProjectManifest,
    scene_id: &str,
) -> Option<super::contracts::NormalizedRect> {
    manifest
        .tracks
        .iter()
        .filter(|track| {
            matches!(
                track.kind,
                super::contracts::TrackKind::Video | super::contracts::TrackKind::Overlay
            )
        })
        .flat_map(|track| &track.clips)
        .find(|clip| clip.scene_id.as_deref() == Some(scene_id) && clip.crop.is_some())
        .and_then(|clip| clip.crop)
}

fn gain_for_track(manifest: &VideoProjectManifest, track_id: &str) -> f64 {
    manifest
        .audio_mix
        .tracks
        .iter()
        .find(|track| track.track_id == track_id)
        .map(|track| f64::from(track.gain_db_milli) / 1_000.0)
        .unwrap_or(if track_id == "music-main" { -12.0 } else { 0.0 })
}

fn canvas_mode(manifest: &VideoProjectManifest) -> &'static str {
    match manifest.layout.mode {
        CanvasMode::Portrait => "portrait",
        CanvasMode::Landscape => "landscape",
        CanvasMode::Square => "square",
        CanvasMode::Custom if manifest.layout.canvas.width == manifest.layout.canvas.height => {
            "square"
        }
        CanvasMode::Custom if manifest.layout.canvas.width < manifest.layout.canvas.height => {
            "portrait"
        }
        CanvasMode::Custom => "landscape",
    }
}

fn aspect_ratio(manifest: &VideoProjectManifest) -> &'static str {
    match canvas_mode(manifest) {
        "portrait" => "9:16",
        "square" => "1:1",
        _ => "16:9",
    }
}

fn present_status(status: Option<&str>) -> &'static str {
    match status.unwrap_or("draft") {
        "ingesting" | "analyzing" => "analyzing",
        "review" => "review",
        "ready" => "editing",
        "rendering" => "rendering",
        "completed" => "exported",
        "failed" => "failed",
        _ => "draft",
    }
}

fn source_mime(source: &SourceAsset) -> &'static str {
    if source.probe.has_video {
        "video/mp4"
    } else if source.probe.has_audio {
        "audio/wav"
    } else {
        "application/octet-stream"
    }
}

fn provenance_label(source: &SourceAsset) -> &'static str {
    match source.kind {
        SourceAssetKind::LocalVideo => "user-selected local media",
        SourceAssetKind::LocalAudio => "user-selected local audio",
        SourceAssetKind::ImportedLink => "authorized exact-link import",
        SourceAssetKind::SoundArSpeech => "existing soundAr speech",
        SourceAssetKind::SoundArMusic => "existing soundAr music",
        SourceAssetKind::SoundArProject => "existing soundAr project",
        SourceAssetKind::Generated => "generated locally",
    }
}

fn artifact_title(artifact: &RenderArtifact) -> &'static str {
    match artifact.role {
        RenderArtifactRole::Proxy => "Source proxy",
        RenderArtifactRole::Thumbnail => "Source thumbnail",
        RenderArtifactRole::Waveform => "Audio waveform",
        RenderArtifactRole::SceneSegment => "Rendered scene",
        RenderArtifactRole::Preview => "Video preview",
        RenderArtifactRole::FinalMaster => "Final video master",
        RenderArtifactRole::Captions => "Captions",
        RenderArtifactRole::Transcript => "Transcript",
        RenderArtifactRole::PublishPackage => "Publish package",
    }
}

fn revision_phase(stage: &RevisionStage) -> &'static str {
    match stage {
        RevisionStage::Ingest => "source",
        RevisionStage::Transcript
        | RevisionStage::Analysis
        | RevisionStage::Plan
        | RevisionStage::Speech
        | RevisionStage::Music
        | RevisionStage::Captions
        | RevisionStage::Tracking => "analyze",
        RevisionStage::SceneRender | RevisionStage::Preview => "preview",
        RevisionStage::FinalRender | RevisionStage::PublishPackage => "export",
    }
}

fn format_label(mime: &str, path: &str) -> &'static str {
    if mime == "video/mp4" || path.to_ascii_lowercase().ends_with(".mp4") {
        "mp4"
    } else if mime == "application/zip" || path.to_ascii_lowercase().ends_with(".zip") {
        "zip"
    } else if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "file"
    }
}

fn managed_path(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn micros_to_millis(value: Microseconds) -> i64 {
    value.0 / 1_000
}

fn required_string(value: &Value, field: &str) -> VideoResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| presentation_error(format!("the stored record is missing {field}")))
}

fn presentation_error(message: impl Into<String>) -> VideoError {
    VideoError::new(VideoErrorCode::InvalidAsset, message)
}

#[cfg(test)]
mod tests {
    use super::super::contracts::{
        AudioMix, CanvasSpec, CaptionCue, LayoutElement, LayoutPlan, NormalizedRect, Provenance,
        ProvenanceKind, RationalFrameRate, ReviewState, ReviewedScene, RevisionRecord, TimeRange,
        TimelineTrack, TrackKind,
    };
    use super::super::visuals::{
        VisualAsset, VisualEasing, VisualFit, VisualLayer, VisualMimeType, VisualMotion,
    };
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn manifest() -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "video-project",
            "Thoughtful reel",
            RationalFrameRate::FPS_30,
            Microseconds(5_000_000),
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
                background_rgba: [18, 18, 18, 255],
                elements: vec![],
            },
            AudioMix {
                target_lufs_milli: -14_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            "2026-08-27T20:00:00.000Z",
        )
        .unwrap();
        manifest.revision = 1;
        manifest.revision_history.push(RevisionRecord {
            id: "revision-1".into(),
            revision: 1,
            parent_id: None,
            actor: "user".into(),
            reason: "Project created".into(),
            changed_paths: vec!["/".into()],
            invalidated_stages: BTreeSet::new(),
            created_at: "2026-08-27T20:00:00.000Z".into(),
        });
        manifest
    }

    #[test]
    fn canonical_project_projects_into_compact_frontend_contract() {
        let manifest = manifest();
        let record = json!({
            "id": "video-project",
            "name": "Thoughtful reel",
            "revision": 1,
            "status": "ready",
            "created_at": "2026-08-27T20:00:00Z",
            "updated_at": "2026-08-27T20:01:00Z",
            "version": {"id": "version-1"},
            "manifest": manifest,
            "assets": [],
            "outputs": [],
        });
        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        assert_eq!(presented["status"], "editing");
        assert_eq!(presented["duration_ms"], 5_000);
        assert_eq!(presented["manifest"]["settings"]["aspect_ratio"], "9:16");
        assert_eq!(presented["manifest"]["source"]["kind"], "prompt");
        let summary = present_video_project_summary(&record, Path::new("/tmp/video-root")).unwrap();
        assert!(summary.get("manifest").is_none());
    }

    #[test]
    fn generated_visuals_project_into_the_authoritative_manifest_and_timeline_lane() {
        let mut manifest = manifest();
        manifest.visual_assets.push(VisualAsset {
            id: "visual-illustration".into(),
            managed_path: "projects/video-project/visuals/visual-illustration.png".into(),
            sha256: "a".repeat(64),
            mime_type: VisualMimeType::Png,
            width: 1080,
            height: 1920,
            has_alpha: true,
            size_bytes: 4096,
            provenance: Provenance {
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: "2026-08-27T20:00:00.000Z".into(),
                producer: "codex-imagegen".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::from([
                    ("generation_id".into(), json!("image-generation-one")),
                    (
                        "prompt".into(),
                        json!("A restrained editorial illustration"),
                    ),
                ]),
            },
            created_at: "2026-08-27T20:00:00.000Z".into(),
        });
        manifest.visual_layers.push(VisualLayer {
            id: "visual-layer-illustration".into(),
            asset_id: "visual-illustration".into(),
            scene_id: None,
            range: TimeRange::new(500_000, 4_500_000).unwrap(),
            fit: VisualFit::Cover,
            crop: None,
            z_index: 3,
            motion: VisualMotion {
                start_bounds: NormalizedRect {
                    x_bp: 0,
                    y_bp: 0,
                    width_bp: 10_000,
                    height_bp: 10_000,
                },
                end_bounds: NormalizedRect {
                    x_bp: 250,
                    y_bp: 250,
                    width_bp: 9_500,
                    height_bp: 9_500,
                },
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: VisualEasing::EaseInOut,
            },
            transition_in_us: Microseconds(250_000),
            transition_out_us: Microseconds(250_000),
        });
        manifest.validate_strict().unwrap();
        let record = json!({
            "id": "video-project",
            "name": "Thoughtful reel",
            "revision": 1,
            "status": "ready",
            "created_at": "2026-08-27T20:00:00Z",
            "updated_at": "2026-08-27T20:01:00Z",
            "version": {"id": "version-1"},
            "manifest": manifest,
            "assets": [],
            "outputs": [],
        });

        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        let visual = &presented["manifest"]["visual_assets"][0];
        assert_eq!(visual["id"], "visual-illustration");
        assert_eq!(visual["mime_type"], "image/png");
        assert_eq!(visual["checksum"], "a".repeat(64));
        assert_eq!(visual["provenance"]["kind"], "generated_locally");
        assert_eq!(
            visual["local_path"],
            "/tmp/video-root/projects/video-project/visuals/visual-illustration.png"
        );
        assert_eq!(presented["poster_url"], visual["local_path"]);
        let layer = &presented["manifest"]["visual_layers"][0];
        assert_eq!(layer["id"], "visual-layer-illustration");
        assert_eq!(layer["start_ms"], 500);
        assert_eq!(layer["end_ms"], 4_500);
        assert_eq!(layer["motion"]["easing"], "ease_in_out");

        let tracks = presented["manifest"]["timeline"]["tracks"]
            .as_array()
            .unwrap();
        let visuals = tracks
            .iter()
            .find(|track| track["kind"] == "visuals")
            .expect("authoritative visuals lane");
        assert_eq!(visuals["items"].as_array().unwrap().len(), 1);
        assert_eq!(visuals["items"][0]["asset_id"], "visual-illustration");
        assert_eq!(visuals["items"][0]["label"], "codex-imagegen");
        assert_eq!(visuals["items"][0]["start_ms"], 500);
        assert_eq!(visuals["items"][0]["end_ms"], 4_500);
    }

    #[test]
    fn authoritative_caption_pages_round_trip_per_scene_and_match_ass_outer_ranges() {
        let mut manifest = manifest();
        manifest.reviewed_scenes = vec![
            ReviewedScene {
                id: "scene-opening".into(),
                candidate_id: None,
                source_asset_id: None,
                source_range: None,
                timeline_start_us: Microseconds::ZERO,
                timeline_duration_us: Microseconds(2_500_000),
                title: "Opening".into(),
                script: "Opening script".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
            ReviewedScene {
                id: "scene-close".into(),
                candidate_id: None,
                source_asset_id: None,
                source_range: None,
                timeline_start_us: Microseconds(2_500_000),
                timeline_duration_us: Microseconds(2_500_000),
                title: "Close".into(),
                script: "Closing script".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
        ];
        manifest.captions = vec![
            CaptionCue {
                id: "caption-opening".into(),
                range: TimeRange::new(100_000, 2_300_000).unwrap(),
                text: "A thoughtful podcast caption keeps the complete idea readable and calmly moves to its next measured page.".into(),
                style_id: "caption-podcast".into(),
                speaker_id: Some("Host".into()),
                transcript_segment_id: None,
                scene_id: Some("scene-opening".into()),
            },
            CaptionCue {
                id: "caption-close".into(),
                range: TimeRange::new(2_600_000, 4_800_000).unwrap(),
                text: "Follow every closing word".into(),
                style_id: "caption-karaoke".into(),
                speaker_id: Some("Guest".into()),
                transcript_segment_id: None,
                scene_id: Some("scene-close".into()),
            },
        ];
        // Persisted cue order is not a presentation clock, and an existing canonical caption
        // track must not create a second UI lane that shadows authoritative pages.
        manifest.captions.reverse();
        manifest.tracks.push(TimelineTrack {
            id: "caption-main".into(),
            kind: TrackKind::Caption,
            clips: vec![],
            preserve_gaps: false,
        });
        let opening_bounds = NormalizedRect {
            x_bp: 100,
            y_bp: 200,
            width_bp: 3_600,
            height_bp: 1_200,
        };
        let close_bounds = NormalizedRect {
            x_bp: 5_000,
            y_bp: 7_000,
            width_bp: 4_500,
            height_bp: 1_800,
        };
        manifest.layout.elements.extend([
            LayoutElement {
                id: "caption-layout-opening".into(),
                role: LayoutRole::Captions,
                scene_id: Some("scene-opening".into()),
                bounds: opening_bounds,
                z_index: 100,
                style_id: None,
            },
            LayoutElement {
                id: "caption-layout-close".into(),
                role: LayoutRole::Captions,
                scene_id: Some("scene-close".into()),
                bounds: close_bounds,
                z_index: 100,
                style_id: None,
            },
        ]);
        manifest.validate_strict().unwrap();
        let canonical_pages = super::super::assembly::plan_caption_preview_pages(&manifest)
            .expect("canonical preview pages");
        assert!(canonical_pages.len() >= 3);
        assert!(canonical_pages
            .windows(2)
            .all(|pair| pair[0].start_us <= pair[1].start_us));
        let ass = super::super::assembly::build_ass_document(
            &manifest,
            &super::super::assembly::AssemblyOptions::default(),
        )
        .expect("ASS document");

        let record = json!({
            "id": "video-project",
            "name": "Thoughtful reel",
            "revision": 1,
            "status": "ready",
            "created_at": "2026-08-27T20:00:00Z",
            "updated_at": "2026-08-27T20:01:00Z",
            "version": {"id": "version-1"},
            "manifest": manifest,
            "assets": [],
            "outputs": [],
        });
        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        assert_eq!(
            presented["manifest"]["scenes"][0]["caption_style"],
            "podcast"
        );
        assert_eq!(
            presented["manifest"]["scenes"][1]["caption_style"],
            "karaoke"
        );
        assert_eq!(
            presented["manifest"]["scenes"][0]["caption_bounds"],
            serde_json::to_value(opening_bounds).unwrap()
        );
        assert_eq!(
            presented["manifest"]["scenes"][1]["caption_bounds"],
            serde_json::to_value(close_bounds).unwrap()
        );
        let presented_pages = presented["manifest"]["caption_pages"]
            .as_array()
            .expect("presented caption pages");
        assert_eq!(presented_pages.len(), canonical_pages.len());
        let caption_tracks = presented["manifest"]["timeline"]["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|track| track["kind"] == "captions")
            .collect::<Vec<_>>();
        assert_eq!(caption_tracks.len(), 1);
        let caption_track = caption_tracks[0];
        assert_eq!(
            caption_track["items"].as_array().unwrap().len(),
            canonical_pages.len()
        );

        for (canonical, page) in canonical_pages.iter().zip(presented_pages) {
            assert_eq!(page["id"], canonical.id);
            assert_eq!(page["cue_id"], canonical.cue_id);
            assert_eq!(page["style_id"], canonical.style_id);
            assert_eq!(page["start_ms"], canonical.start_us.0 / 1_000);
            assert_eq!(page["end_ms"], canonical.end_us.0 / 1_000);
            assert_eq!(page["text"], canonical.text);
            assert_eq!(
                page["bounds"],
                serde_json::to_value(canonical.bounds).unwrap()
            );
            assert_eq!(page["font_size_bp"], canonical.font_size_bp);
            let timeline_item = caption_track["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == canonical.id)
                .expect("authoritative caption timeline item");
            assert_eq!(timeline_item["bounds"], page["bounds"]);
            assert_eq!(timeline_item["font_size_bp"], page["font_size_bp"]);
            if canonical.style_id == "podcast" {
                let start = ass_time_for_test(canonical.start_us);
                let end = ass_time_for_test(canonical.end_us);
                let event = ass.lines().find(|line| {
                    line.starts_with(&format!("Dialogue: 10,{start},{end},CaptionPodcast,"))
                });
                assert!(event.is_some(), "missing ASS page {start}..{end}");
                let event_text = event.unwrap().splitn(10, ',').nth(9).unwrap();
                assert_eq!(
                    strip_ass_overrides(event_text).replace(r"\N", "\n"),
                    canonical.text
                );
            }
        }
        assert!(ass.contains(r"{\an5\pos(137,102)\fs"));
        assert!(ass.contains(r"{\an5\pos(522,1011)\fs"));
    }

    fn strip_ass_overrides(value: &str) -> String {
        let mut result = String::new();
        let mut in_override = false;
        for character in value.chars() {
            match character {
                '{' if !in_override => in_override = true,
                '}' if in_override => in_override = false,
                _ if !in_override => result.push(character),
                _ => {}
            }
        }
        result
    }

    fn ass_time_for_test(value: Microseconds) -> String {
        let total_centiseconds = value.0.max(0) / 10_000;
        let hours = total_centiseconds / 360_000;
        let minutes = (total_centiseconds / 6_000) % 60;
        let seconds = (total_centiseconds / 100) % 60;
        let centiseconds = total_centiseconds % 100;
        format!("{hours}:{minutes:02}:{seconds:02}.{centiseconds:02}")
    }

    #[test]
    fn primary_output_is_playable_master_not_an_opaque_path() {
        let record = json!({
            "id": "video-project",
            "name": "Thoughtful reel",
            "revision": 1,
            "status": "completed",
            "created_at": "2026-08-27T20:00:00Z",
            "updated_at": "2026-08-27T20:01:00Z",
            "version": {"id": "version-1"},
            "manifest": manifest(),
            "assets": [],
            "outputs": [
                {
                    "id": "master-1", "version_id": "version-1", "kind": "final_master",
                    "label": "Portrait master", "artifact_path": "/tmp/video-root/master.mp4",
                    "mime_type": "video/mp4", "size_bytes": 42, "sha256": "a".repeat(64),
                    "duration_us": 5_000_000, "width": 1080, "height": 1920,
                    "is_primary": true, "created_at": "2026-08-27T20:01:00Z"
                },
                {
                    "id": "variation-2", "version_id": "version-1", "kind": "variation",
                    "label": "Variation 2", "artifact_path": "/tmp/video-root/variation-2.mp4",
                    "mime_type": "video/mp4", "size_bytes": 40, "sha256": "b".repeat(64),
                    "duration_us": 5_000_000, "width": 1080, "height": 1920,
                    "is_primary": false, "created_at": "2026-08-27T20:01:01Z"
                },
                {
                    "id": "publish-1", "version_id": "version-1", "kind": "publish_package",
                    "label": "Publish package", "artifact_path": "/tmp/video-root/publish.zip",
                    "mime_type": "application/zip", "size_bytes": 84, "sha256": "c".repeat(64),
                    "is_primary": false, "created_at": "2026-08-27T20:01:02Z"
                }
            ],
        });
        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        assert_eq!(presented["master"]["role"], "master");
        assert_eq!(presented["master"]["playable"], true);
        assert_eq!(
            presented["master"]["local_path"],
            "/tmp/video-root/master.mp4"
        );
        assert_eq!(presented["manifest"]["artifacts"][0]["role"], "master");
        assert_eq!(presented["deliverables"].as_array().unwrap().len(), 3);
        assert_eq!(presented["deliverables"][1]["role"], "variation");
        assert_eq!(presented["deliverables"][2]["role"], "publish-package");
        let summary = present_video_project_summary(&record, Path::new("/tmp/video-root")).unwrap();
        assert_eq!(summary["deliverables"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_master_from_an_older_version_is_not_presented_as_current() {
        let record = json!({
            "id": "video-project",
            "name": "Thoughtful reel",
            "revision": 2,
            "status": "ready",
            "created_at": "2026-08-27T20:00:00Z",
            "updated_at": "2026-08-27T20:02:00Z",
            "version": {"id": "version-2"},
            "manifest": manifest(),
            "assets": [],
            "outputs": [{
                "id": "master-old", "version_id": "version-1", "kind": "master",
                "label": "Stale master", "artifact_path": "/tmp/video-root/old.mp4",
                "mime_type": "video/mp4", "size_bytes": 42, "sha256": "a".repeat(64),
                "duration_us": 5_000_000, "width": 1080, "height": 1920,
                "is_primary": true, "created_at": "2026-08-27T20:01:00Z"
            }],
        });
        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        assert!(presented["master"].is_null());
        assert!(presented["deliverables"].as_array().unwrap().is_empty());
        assert!(presented["manifest"]["artifacts"]
            .as_array()
            .expect("artifact list")
            .is_empty());
    }
}
