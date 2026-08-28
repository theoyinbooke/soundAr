//! Transport presentation for the native Video Studio surface.
//!
//! Persistence and rendering use the strict canonical manifest. The React workspace needs a
//! compact, media-oriented projection with millisecond display clocks and directly playable
//! artifact paths. Keeping this adapter pure prevents the UI, Tauri commands, and agent tools
//! from growing separate workflow implementations.

use super::contracts::{
    CandidateStatus, CanvasMode, LayoutRole, Microseconds, PublicationState, RenderArtifact,
    RenderArtifactRole, RevisionStage, SourceAsset, SourceAssetKind, TrackKind, VideoError,
    VideoErrorCode, VideoProjectManifest, VideoResult,
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
        .cloned()
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

    let mut artifacts = Vec::new();
    let mut artifact_ids = BTreeSet::new();
    for output in &outputs {
        let artifact = present_output(output, &project_id, &version_id)?;
        if let Some(id) = artifact.get("id").and_then(Value::as_str) {
            artifact_ids.insert(id.to_string());
        }
        artifacts.push(artifact);
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

    let master = outputs
        .iter()
        .find(|output| output.get("is_primary").and_then(Value::as_bool) == Some(true))
        .map(|output| present_output(output, &project_id, &version_id))
        .transpose()?;
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
    let layout = canvas_mode(&manifest);
    let scenes = manifest
        .reviewed_scenes
        .iter()
        .enumerate()
        .map(|(index, scene)| {
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
                "captions_enabled": manifest.captions.iter().any(|caption| caption.scene_id.as_deref() == Some(scene.id.as_str())),
                "caption_style": caption_style,
                "voice_gain_db": gain_for_track(&manifest, "audio-main"),
                "music_gain_db": gain_for_track(&manifest, "music-main"),
            })
        })
        .collect::<Vec<_>>();
    let timeline = present_timeline(&manifest);
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
    let poster_url = artifacts
        .iter()
        .find(|artifact| artifact.get("format").and_then(Value::as_str) == Some("image"))
        .and_then(|artifact| artifact.get("local_path"))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "id": project_id,
        "name": name,
        "status": status,
        "duration_ms": duration_ms,
        "scene_count": manifest.reviewed_scenes.len(),
        "updated_at": updated_at,
        "poster_url": poster_url,
        "master": master,
        "created_at": created_at,
        "manifest": {
            "schema_version": 1,
            "version_id": version_id,
            "source": source,
            "transcript_version": manifest.transcript.as_ref().map(|transcript| transcript.id.as_str()).unwrap_or(""),
            "transcript": transcript_segments,
            "candidates": candidates,
            "scenes": scenes,
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
            SourceAssetKind::SoundArSpeech | SourceAssetKind::SoundArMusic | SourceAssetKind::SoundArProject => "audio",
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

fn present_timeline(manifest: &VideoProjectManifest) -> Value {
    let source_kind = manifest
        .source_assets
        .iter()
        .map(|source| (source.id.as_str(), &source.kind))
        .collect::<BTreeMap<_, _>>();
    let mut tracks = Vec::new();
    for track in &manifest.tracks {
        let frontend_kind = match track.kind {
            TrackKind::Video | TrackKind::Overlay => "video",
            TrackKind::Caption => "captions",
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
    if !manifest.captions.is_empty() {
        tracks.push(json!({
            "kind": "captions",
            "items": manifest.captions.iter().map(|caption| json!({
                "id": caption.id,
                "track": "captions",
                "kind": "clip",
                "start_ms": micros_to_millis(caption.range.start_us),
                "end_ms": micros_to_millis(caption.range.end_us),
                "label": caption.text,
                "scene_id": caption.scene_id,
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
    let style = manifest
        .captions
        .first()
        .map(|caption| caption.style_id.to_ascii_lowercase())
        .unwrap_or_default();
    if style.contains("kinetic") {
        "kinetic"
    } else if style.contains("calm") {
        "calm"
    } else {
        "clean-white"
    }
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
        AudioMix, CanvasSpec, LayoutPlan, NormalizedRect, RationalFrameRate, RevisionRecord,
    };
    use super::*;
    use std::collections::BTreeSet;

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
            "outputs": [{
                "id": "master-1", "version_id": "version-1", "kind": "final_master",
                "label": "Portrait master", "artifact_path": "/tmp/video-root/master.mp4",
                "mime_type": "video/mp4", "size_bytes": 42, "sha256": "a".repeat(64),
                "duration_us": 5_000_000, "width": 1080, "height": 1920,
                "is_primary": true, "created_at": "2026-08-27T20:01:00Z"
            }],
        });
        let presented = present_video_project(&record, Path::new("/tmp/video-root")).unwrap();
        assert_eq!(presented["master"]["role"], "master");
        assert_eq!(presented["master"]["playable"], true);
        assert_eq!(
            presented["master"]["local_path"],
            "/tmp/video-root/master.mp4"
        );
        assert_eq!(presented["manifest"]["artifacts"][0]["role"], "master");
    }
}
