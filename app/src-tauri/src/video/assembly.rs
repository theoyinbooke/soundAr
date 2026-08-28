//! FFmpeg assembly planning for reviewed Video Studio timelines.
//!
//! Media timing is read exclusively from canonical timeline tracks. Each clip is sought from its
//! original source clock, deliberate timeline holes become explicit black/silent segments, and
//! scene metadata is reserved for title cards. Captions and speaker cards are burned from a
//! generated ASS document. Commands never involve a shell, and both NVENC and software plans are
//! deterministic.

use super::contracts::{
    AudioMixTrack, CanvasMode, CaptionCue, LayoutPlan, LayoutRole, Microseconds, NormalizedRect,
    RationalRate, RenderArtifact, SourceAsset, TimeRange, TimelineClip, TimelineTrack, TrackKind,
    VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
};
use super::media::local_media_input_args;
use super::renderer::{
    PortraitLayout, RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass,
    VideoEncoder,
};
use super::timeline::{partition_track, TimelineSpanKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionTheme {
    CleanWhite,
    Calm,
    Kinetic,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssemblyOptions {
    pub profile: RenderProfile,
    pub portrait_layout: PortraitLayout,
    pub caption_theme: CaptionTheme,
    pub include_title_cards: bool,
    pub include_speaker_cards: bool,
    pub burn_captions: bool,
}

impl Default for AssemblyOptions {
    fn default() -> Self {
        Self {
            profile: RenderProfile::Preview,
            portrait_layout: PortraitLayout::CenterCrop,
            caption_theme: CaptionTheme::CleanWhite,
            include_title_cards: true,
            include_speaker_cards: true,
            burn_captions: true,
        }
    }
}

/// Builds one deterministic FFmpeg plan for the reviewed timeline. Scene-level cache callers can
/// use the same clip/range data to pre-render unchanged segments, then bind those artifacts back
/// into canonical tracks without changing timeline math.
pub fn build_timeline_render_plan(
    ffmpeg: &Path,
    manifest: &VideoProjectManifest,
    resolved_sources: &BTreeMap<String, PathBuf>,
    subtitles_path: Option<&Path>,
    output: &Path,
    options: &AssemblyOptions,
    h264_nvenc_runtime: bool,
) -> VideoResult<RenderCommandPlan> {
    manifest.validate_strict()?;
    let ffmpeg = validate_executable(ffmpeg)?;
    let output = validate_output_path(output)?;
    let subtitles = subtitles_path
        .map(fs::canonicalize)
        .transpose()
        .map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidCaption,
                format!("caption document could not be opened: {error}"),
            )
        })?;
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
    let video_tracks = manifest
        .tracks
        .iter()
        .filter(|track| matches!(track.kind, TrackKind::Video))
        .collect::<Vec<_>>();
    if video_tracks.len() > 1 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTrack,
            "assembly supports one canonical primary video track",
        ));
    }
    if manifest.tracks.iter().any(|track| {
        matches!(track.kind, TrackKind::Overlay | TrackKind::Caption) && !track.clips.is_empty()
    }) {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTrack,
            "overlay and caption media tracks require an explicit composition binding; use layout elements and manifest captions",
        ));
    }
    let primary_video = video_tracks.first().copied();
    let audio_tracks = manifest
        .tracks
        .iter()
        .filter(|track| matches!(track.kind, TrackKind::Audio))
        .collect::<Vec<_>>();

    let (width, height) = profile_dimensions(options.profile, &manifest.layout);
    let frame_rate = format!(
        "{}/{}",
        manifest.frame_rate.numerator, manifest.frame_rate.denominator
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

    let mut input_by_clip = BTreeMap::new();
    let input_tracks = primary_video
        .into_iter()
        .chain(audio_tracks.iter().copied())
        .collect::<Vec<_>>();
    for track in input_tracks {
        for clip in &track.clips {
            if input_by_clip.contains_key(clip.id.as_str()) {
                continue;
            }
            let (media_namespace, media_id) = clip_media_identity(clip);
            let namespaced_id = format!("{media_namespace}:{media_id}");
            let path = resolved_sources
                .get(&namespaced_id)
                .or_else(|| resolved_sources.get(media_id))
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        format!("timeline media {media_id} has no resolved managed file"),
                    )
                })?;
            let path = fs::canonicalize(path).map_err(|error| {
                VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    format!("timeline media {media_id} could not be opened: {error}"),
                )
            })?;
            if !path.is_file() {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    format!("timeline media {media_id} is not a regular file"),
                ));
            }
            let input_index = input_by_clip.len();
            input_by_clip.insert(clip.id.as_str(), input_index);
            args.extend([
                OsString::from("-ss"),
                OsString::from(ffmpeg_time(clip.source_range.start_us)),
                OsString::from("-t"),
                OsString::from(ffmpeg_time(clip.source_range.duration()?)),
            ]);
            args.extend(local_media_input_args(&path).map_err(|error| {
                VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    format!(
                        "timeline media {media_id} failed local-input policy: {}",
                        error.message
                    ),
                )
            })?);
        }
    }
    args.extend([
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-map_chapters"),
        OsString::from("-1"),
    ]);

    let mut filters = Vec::new();
    let background = background_color(&manifest.layout);
    let mut assembled_video = if let Some(track) = primary_video {
        append_video_track_filters(
            &mut filters,
            track,
            &manifest.gaps,
            manifest.timeline_duration_us,
            &input_by_clip,
            width,
            height,
            &frame_rate,
            options.portrait_layout,
            &background,
        )?
    } else {
        filters.push(format!(
            "color=c={background}:s={width}x{height}:r={frame_rate}:d={}[assembled_video]",
            ffmpeg_time(manifest.timeline_duration_us)
        ));
        "assembled_video".to_string()
    };

    let mut audio_labels = Vec::new();
    for (index, track) in audio_tracks.iter().enumerate() {
        let mix = manifest
            .audio_mix
            .tracks
            .iter()
            .find(|mix| mix.track_id == track.id);
        audio_labels.push(append_audio_track_filters(
            &mut filters,
            track,
            &manifest.gaps,
            manifest.timeline_duration_us,
            &input_by_clip,
            &source_by_id,
            &artifact_by_id,
            mix,
            index,
        )?);
    }
    // Imported A/V sources initially use one video track. Until planning splits
    // that media into canonical video + audio tracks, retain its embedded audio
    // without consulting scene metadata.
    if audio_labels.is_empty() {
        if let Some(track) = primary_video {
            audio_labels.push(append_audio_track_filters(
                &mut filters,
                track,
                &manifest.gaps,
                manifest.timeline_duration_us,
                &input_by_clip,
                &source_by_id,
                &artifact_by_id,
                None,
                0,
            )?);
        }
    }
    let mut assembled_audio = match audio_labels.as_slice() {
        [] => {
            filters.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=duration={},asetpts=PTS-STARTPTS[assembled_audio]",
                ffmpeg_time(manifest.timeline_duration_us)
            ));
            "assembled_audio".to_string()
        }
        [only] => only.clone(),
        labels => {
            let inputs = labels
                .iter()
                .map(|label| format!("[{label}]"))
                .collect::<String>();
            filters.push(format!(
                "{inputs}amix=inputs={}:normalize=0:dropout_transition=0,atrim=duration={},asetpts=PTS-STARTPTS[assembled_audio]",
                labels.len(),
                ffmpeg_time(manifest.timeline_duration_us)
            ));
            "assembled_audio".to_string()
        }
    };
    let waveform_requested = primary_video.is_none_or(|track| track.clips.is_empty())
        || manifest
            .layout
            .elements
            .iter()
            .any(|element| matches!(element.role, LayoutRole::Waveform));
    let has_media_audio = audio_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .any(|clip| clip_media_has_audio(clip, &source_by_id, &artifact_by_id))
        || (audio_tracks.is_empty()
            && primary_video.is_some_and(|track| {
                track
                    .clips
                    .iter()
                    .any(|clip| clip_media_has_audio(clip, &source_by_id, &artifact_by_id))
            }));
    if waveform_requested && has_media_audio {
        let waveform_width = width.saturating_sub(width / 6).max(2).min(width);
        let waveform_height = (height / 4).max(2).min(height);
        filters.push(format!(
            "[{assembled_audio}]asplit=2[waveform_audio][audio_for_master]"
        ));
        filters.push(format!(
            "[waveform_audio]showwaves=s={waveform_width}x{waveform_height}:mode=line:colors=0xE5E7EB@0.75:rate={frame_rate},format=rgba[waveform_visual]"
        ));
        filters.push(format!(
            "[{assembled_video}][waveform_visual]overlay=(W-w)/2:(H-h)/2:shortest=1[assembled_video_waveform]"
        ));
        assembled_video = "assembled_video_waveform".into();
        assembled_audio = "audio_for_master".into();
    }
    let video_output = if options.burn_captions {
        if let Some(path) = subtitles.as_deref() {
            filters.push(format!(
                "[{assembled_video}]subtitles=filename='{}'[video_output]",
                escape_filter_path(path)
            ));
            "[video_output]"
        } else {
            "[assembled_video]"
        }
    } else {
        "[assembled_video]"
    };
    let target_lufs = f64::from(manifest.audio_mix.target_lufs_milli) / 1_000.0;
    let true_peak = f64::from(manifest.audio_mix.true_peak_db_milli) / 1_000.0;
    filters.push(format!(
        "[{assembled_audio}]loudnorm=I={target_lufs:.1}:TP={true_peak:.1}:LRA=11[audio_output]"
    ));
    args.extend([
        OsString::from("-filter_complex"),
        OsString::from(filters.join(";")),
        OsString::from("-map"),
        OsString::from(video_output),
        OsString::from("-map"),
        OsString::from("[audio_output]"),
    ]);

    let build_command = |encoder| RenderCommand {
        program: ffmpeg.clone(),
        args: render_output_arguments(args.clone(), output.clone(), options.profile, encoder),
        output: output.clone(),
        encoder,
        emits_progress: true,
    };
    let primary_encoder = if h264_nvenc_runtime {
        VideoEncoder::H264Nvenc
    } else {
        VideoEncoder::Libx264
    };
    Ok(RenderCommandPlan {
        profile: options.profile,
        workload_class: match options.profile {
            RenderProfile::Proxy => RenderWorkloadClass::Light,
            RenderProfile::Preview => RenderWorkloadClass::Medium,
            RenderProfile::Final => RenderWorkloadClass::Heavy,
        },
        primary: build_command(primary_encoder),
        software_fallback: h264_nvenc_runtime.then(|| build_command(VideoEncoder::Libx264)),
    })
}

/// Generate the caption/title/speaker overlay document from timeline-clock cues. The document is
/// deterministic for a manifest revision and can be content-addressed independently of video.
pub fn build_ass_document(
    manifest: &VideoProjectManifest,
    options: &AssemblyOptions,
) -> VideoResult<String> {
    manifest.validate_strict()?;
    let (width, height) = profile_dimensions(options.profile, &manifest.layout);
    let font_size = match options.caption_theme {
        CaptionTheme::CleanWhite => (height as f64 * 0.044).round() as u32,
        CaptionTheme::Calm => (height as f64 * 0.036).round() as u32,
        CaptionTheme::Kinetic => (height as f64 * 0.052).round() as u32,
    }
    .clamp(24, 88);
    let margin_v = (height as f64 * 0.09).round() as u32;
    let mut document = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nScaledBorderAndShadow: yes\nWrapStyle: 0\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Caption,Inter,{font_size},&H00FFFFFF,&H00FFFFFF,&H78000000,&H3C000000,-1,0,0,0,100,100,0,0,1,3,0,2,70,70,{margin_v},1\nStyle: Title,Inter,{},&H00FFFFFF,&H00FFFFFF,&H90000000,&H50000000,-1,0,0,0,100,100,0,0,1,3,0,8,90,90,{},1\nStyle: Speaker,Inter,{},&H00FFFFFF,&H00FFFFFF,&H80000000,&H50000000,-1,0,0,0,100,100,0,0,3,1,0,7,70,70,70,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        (font_size as f64 * 1.08).round() as u32,
        (height as f64 * 0.07).round() as u32,
        (font_size as f64 * 0.68).round() as u32,
    );
    for caption in &manifest.captions {
        document.push_str(&ass_dialogue(
            10,
            caption.range.start_us,
            caption.range.end_us,
            "Caption",
            caption.speaker_id.as_deref().unwrap_or_default(),
            &caption_text(caption, options.caption_theme),
        ));
    }
    if options.include_title_cards {
        for scene in &manifest.reviewed_scenes {
            let end = scene
                .timeline_start_us
                .checked_add(Microseconds(scene.timeline_duration_us.0.min(1_800_000)))?;
            document.push_str(&ass_dialogue(
                20,
                scene.timeline_start_us,
                end,
                "Title",
                "",
                &format!("{{\\fad(100,220)}}{}", escape_ass_text(&scene.title)),
            ));
        }
    }
    if options.include_speaker_cards {
        append_speaker_cards(&mut document, &manifest.captions)?;
    }
    Ok(document)
}

pub fn write_ass_document_atomic(path: &Path, document: &str) -> VideoResult<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption document path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        VideoError::new(
            VideoErrorCode::InvalidCaption,
            format!("caption directory could not be created: {error}"),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::InvalidCaption,
                "caption document path has no valid filename",
            )
        })?;
    let staging = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| {
                VideoError::new(
                    VideoErrorCode::InvalidCaption,
                    format!("caption staging file could not be created: {error}"),
                )
            })?;
        file.write_all(document.as_bytes()).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidCaption,
                format!("caption document could not be written: {error}"),
            )
        })?;
        file.sync_all().map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidCaption,
                format!("caption document could not be synchronized: {error}"),
            )
        })?;
        fs::rename(&staging, path).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidCaption,
                format!("caption document could not be published atomically: {error}"),
            )
        })?;
        Ok(path.to_path_buf())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result
}

fn clip_media_identity(clip: &TimelineClip) -> (&'static str, &str) {
    if let Some(source_id) = clip.media.source_asset_id.as_deref() {
        return ("source", source_id);
    }
    (
        "artifact",
        clip.media
            .render_artifact_id
            .as_deref()
            .expect("strict manifest media references select exactly one identifier"),
    )
}

enum RenderSpan<'a> {
    Clip(&'a TimelineClip),
    Gap(TimeRange),
}

fn complete_track_spans<'a>(
    track: &'a TimelineTrack,
    gaps: &[super::contracts::TimelineGap],
    duration: Microseconds,
) -> VideoResult<Vec<RenderSpan<'a>>> {
    let clips = track
        .clips
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect::<BTreeMap<_, _>>();
    let canonical = partition_track(track, gaps, duration)?;
    let mut completed = Vec::with_capacity(canonical.len().saturating_mul(2).saturating_add(1));
    let mut cursor = Microseconds::ZERO;
    for span in canonical {
        if span.range.start_us > cursor {
            completed.push(RenderSpan::Gap(TimeRange::new(
                cursor.0,
                span.range.start_us.0,
            )?));
        }
        match span.kind {
            TimelineSpanKind::Clip { clip_id } => completed.push(RenderSpan::Clip(
                clips.get(clip_id.as_str()).copied().ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "canonical track partition references a missing clip",
                    )
                })?,
            )),
            TimelineSpanKind::Gap { .. } => completed.push(RenderSpan::Gap(span.range)),
        }
        cursor = span.range.end_us;
    }
    if cursor < duration {
        completed.push(RenderSpan::Gap(TimeRange::new(cursor.0, duration.0)?));
    }
    if completed.is_empty() {
        completed.push(RenderSpan::Gap(TimeRange::new(0, duration.0)?));
    }
    Ok(completed)
}

#[allow(clippy::too_many_arguments)]
fn append_video_track_filters(
    filters: &mut Vec<String>,
    track: &TimelineTrack,
    gaps: &[super::contracts::TimelineGap],
    timeline_duration: Microseconds,
    input_by_clip: &BTreeMap<&str, usize>,
    width: u32,
    height: u32,
    frame_rate: &str,
    layout: PortraitLayout,
    background: &str,
) -> VideoResult<String> {
    let spans = complete_track_spans(track, gaps, timeline_duration)?;
    let mut labels = Vec::with_capacity(spans.len());
    for (index, span) in spans.into_iter().enumerate() {
        let label = format!("vseg{index}");
        match span {
            RenderSpan::Clip(clip) => {
                let input_index = input_by_clip.get(clip.id.as_str()).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "video clip has no resolved FFmpeg input",
                    )
                })?;
                append_video_clip_filter(
                    filters,
                    *input_index,
                    clip,
                    &label,
                    width,
                    height,
                    frame_rate,
                    layout,
                    background,
                    index,
                );
            }
            RenderSpan::Gap(range) => filters.push(format!(
                "color=c={background}:s={width}x{height}:r={frame_rate}:d={}[{label}]",
                ffmpeg_time(range.duration()?)
            )),
        }
        labels.push(label);
    }
    let inputs = labels
        .iter()
        .map(|label| format!("[{label}]"))
        .collect::<String>();
    filters.push(format!(
        "{inputs}concat=n={}:v=1:a=0[assembled_video]",
        labels.len()
    ));
    Ok("assembled_video".into())
}

#[allow(clippy::too_many_arguments)]
fn append_video_clip_filter(
    filters: &mut Vec<String>,
    input_index: usize,
    clip: &TimelineClip,
    output_label: &str,
    width: u32,
    height: u32,
    frame_rate: &str,
    layout: PortraitLayout,
    background: &str,
    segment_index: usize,
) {
    let duration = ffmpeg_time(clip.timeline_duration_us);
    let mut prefix = format!(
        "[{input_index}:v:0]setpts=(PTS-STARTPTS)*{}/{},trim=duration={duration},setpts=PTS-STARTPTS",
        clip.playback_rate.denominator, clip.playback_rate.numerator
    );
    if let Some(crop) = clip.crop {
        prefix.push(',');
        prefix.push_str(&normalized_crop_filter(crop));
    }
    match layout {
        PortraitLayout::CenterCrop => filters.push(format!(
            "{prefix},scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},setsar=1,fps={frame_rate}[{output_label}]"
        )),
        PortraitLayout::Contain => filters.push(format!(
            "{prefix},scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color={background},setsar=1,fps={frame_rate}[{output_label}]"
        )),
        PortraitLayout::BlurPad => filters.push(format!(
            "{prefix},split=2[vbg{segment_index}][vfg{segment_index}];[vbg{segment_index}]scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},boxblur=luma_radius=24:luma_power=1[bg{segment_index}];[vfg{segment_index}]scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2[fg{segment_index}];[bg{segment_index}][fg{segment_index}]overlay=(W-w)/2:(H-h)/2,setsar=1,fps={frame_rate}[{output_label}]"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn append_audio_track_filters(
    filters: &mut Vec<String>,
    track: &TimelineTrack,
    gaps: &[super::contracts::TimelineGap],
    timeline_duration: Microseconds,
    input_by_clip: &BTreeMap<&str, usize>,
    source_by_id: &BTreeMap<&str, &SourceAsset>,
    artifact_by_id: &BTreeMap<&str, &RenderArtifact>,
    mix: Option<&AudioMixTrack>,
    track_index: usize,
) -> VideoResult<String> {
    let spans = complete_track_spans(track, gaps, timeline_duration)?;
    let mut labels = Vec::with_capacity(spans.len());
    for (span_index, span) in spans.into_iter().enumerate() {
        let label = format!("atrack{track_index}seg{span_index}");
        match span {
            RenderSpan::Clip(clip) if clip_media_has_audio(clip, source_by_id, artifact_by_id) => {
                let input_index = input_by_clip.get(clip.id.as_str()).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "audio clip has no resolved FFmpeg input",
                    )
                })?;
                let tempo = atempo_filters(clip.playback_rate);
                let gain = if clip.muted {
                    "volume=0".to_string()
                } else {
                    format!("volume={:.3}dB", f64::from(clip.gain_db_milli) / 1_000.0)
                };
                filters.push(format!(
                    "[{input_index}:a:0]asetpts=PTS-STARTPTS,{tempo},atrim=duration={},asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,{gain}[{label}]",
                    ffmpeg_time(clip.timeline_duration_us)
                ));
            }
            RenderSpan::Clip(clip) => filters.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=duration={},asetpts=PTS-STARTPTS[{label}]",
                ffmpeg_time(clip.timeline_duration_us)
            )),
            RenderSpan::Gap(range) => filters.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=duration={},asetpts=PTS-STARTPTS[{label}]",
                ffmpeg_time(range.duration()?)
            )),
        }
        labels.push(label);
    }
    let inputs = labels
        .iter()
        .map(|label| format!("[{label}]"))
        .collect::<String>();
    let raw_label = format!("atrack{track_index}raw");
    filters.push(format!(
        "{inputs}concat=n={}:v=0:a=1[{raw_label}]",
        labels.len()
    ));
    let output = format!("atrack{track_index}");
    let gain_db_milli = mix.map_or(0, |mix| mix.gain_db_milli);
    let pan_milli = mix.map_or(0, |mix| mix.pan_milli);
    let mut chain = format!(
        "[{raw_label}]volume={:.3}dB",
        f64::from(gain_db_milli) / 1_000.0
    );
    if pan_milli != 0 {
        let pan = f64::from(pan_milli) / 1_000.0;
        let left = if pan > 0.0 { 1.0 - pan } else { 1.0 };
        let right = if pan < 0.0 { 1.0 + pan } else { 1.0 };
        chain.push_str(&format!(",pan=stereo|c0={left:.6}*c0|c1={right:.6}*c1"));
    }
    chain.push_str(&format!("[{output}]"));
    filters.push(chain);
    Ok(output)
}

fn clip_media_has_audio(
    clip: &TimelineClip,
    source_by_id: &BTreeMap<&str, &SourceAsset>,
    artifact_by_id: &BTreeMap<&str, &RenderArtifact>,
) -> bool {
    if let Some(source_id) = clip.media.source_asset_id.as_deref() {
        return source_by_id
            .get(source_id)
            .is_some_and(|source| source.probe.has_audio);
    }
    clip.media
        .render_artifact_id
        .as_deref()
        .and_then(|id| artifact_by_id.get(id))
        .is_some_and(|artifact| {
            artifact.mime_type.starts_with("audio/") || artifact.mime_type.starts_with("video/")
        })
}

fn atempo_filters(rate: RationalRate) -> String {
    let mut remaining = f64::from(rate.numerator) / f64::from(rate.denominator);
    let mut filters = Vec::new();
    while remaining > 2.0 {
        filters.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 {
        filters.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    if (remaining - 1.0).abs() > f64::EPSILON || filters.is_empty() {
        filters.push(format!("atempo={remaining:.8}"));
    }
    filters.join(",")
}

fn normalized_crop_filter(crop: NormalizedRect) -> String {
    format!(
        "crop=w='max(2\\,trunc(iw*{}/20000)*2)':h='max(2\\,trunc(ih*{}/20000)*2)':x='trunc(iw*{}/10000/2)*2':y='trunc(ih*{}/10000/2)*2'",
        crop.width_bp, crop.height_bp, crop.x_bp, crop.y_bp
    )
}

fn background_color(layout: &LayoutPlan) -> String {
    let [red, green, blue, _] = layout.background_rgba;
    format!("0x{red:02X}{green:02X}{blue:02X}")
}

fn render_output_arguments(
    mut args: Vec<OsString>,
    output: PathBuf,
    profile: RenderProfile,
    encoder: VideoEncoder,
) -> Vec<OsString> {
    match encoder {
        VideoEncoder::H264Nvenc => args.extend([
            OsString::from("-c:v"),
            OsString::from("h264_nvenc"),
            OsString::from("-preset"),
            OsString::from(match profile {
                RenderProfile::Proxy => "p2",
                RenderProfile::Preview => "p3",
                RenderProfile::Final => "p5",
            }),
            OsString::from("-tune"),
            OsString::from("hq"),
            OsString::from("-rc"),
            OsString::from("vbr"),
            OsString::from("-cq"),
            OsString::from(match profile {
                RenderProfile::Proxy => "29",
                RenderProfile::Preview => "24",
                RenderProfile::Final => "19",
            }),
            OsString::from("-b:v"),
            OsString::from("0"),
        ]),
        VideoEncoder::Libx264 => args.extend([
            OsString::from("-c:v"),
            OsString::from("libx264"),
            OsString::from("-preset"),
            OsString::from(match profile {
                RenderProfile::Proxy => "ultrafast",
                RenderProfile::Preview => "veryfast",
                RenderProfile::Final => "medium",
            }),
            OsString::from("-crf"),
            OsString::from(match profile {
                RenderProfile::Proxy => "28",
                RenderProfile::Preview => "24",
                RenderProfile::Final => "19",
            }),
        ]),
        VideoEncoder::Image => {}
    }
    args.extend([
        OsString::from("-pix_fmt"),
        OsString::from("yuv420p"),
        OsString::from("-c:a"),
        OsString::from("aac"),
        OsString::from("-b:a"),
        OsString::from(match profile {
            RenderProfile::Proxy => "96k",
            RenderProfile::Preview => "128k",
            RenderProfile::Final => "192k",
        }),
        OsString::from("-ar"),
        OsString::from("48000"),
        OsString::from("-movflags"),
        OsString::from("+faststart"),
        OsString::from("-shortest"),
        OsString::from("-f"),
        OsString::from("mp4"),
        output.as_os_str().to_os_string(),
    ]);
    args
}

fn append_speaker_cards(document: &mut String, captions: &[CaptionCue]) -> VideoResult<()> {
    let mut prior_speaker: Option<&str> = None;
    for caption in captions {
        let Some(speaker) = caption
            .speaker_id
            .as_deref()
            .map(str::trim)
            .filter(|speaker| !speaker.is_empty())
        else {
            continue;
        };
        if prior_speaker == Some(speaker) {
            continue;
        }
        let end = caption
            .range
            .start_us
            .checked_add(Microseconds(1_600_000))?;
        let end = end.min(caption.range.end_us);
        document.push_str(&ass_dialogue(
            30,
            caption.range.start_us,
            end,
            "Speaker",
            speaker,
            &format!("{{\\fad(80,180)}}{}", escape_ass_text(speaker)),
        ));
        prior_speaker = Some(speaker);
    }
    Ok(())
}

fn caption_text(caption: &CaptionCue, theme: CaptionTheme) -> String {
    let text = escape_ass_text(&caption.text);
    match theme {
        CaptionTheme::CleanWhite => text,
        CaptionTheme::Calm => format!("{{\\fad(120,160)}}{text}"),
        CaptionTheme::Kinetic => format!("{{\\fad(60,90)\\fscx104\\fscy104}}{text}"),
    }
}

fn ass_dialogue(
    layer: u8,
    start: Microseconds,
    end: Microseconds,
    style: &str,
    name: &str,
    text: &str,
) -> String {
    format!(
        "Dialogue: {layer},{},{},{style},{},0,0,0,,{}\n",
        ass_time(start),
        ass_time(end),
        escape_ass_field(name),
        text
    )
}

fn ass_time(value: Microseconds) -> String {
    let total_centiseconds = value.0.max(0) / 10_000;
    let hours = total_centiseconds / 360_000;
    let minutes = (total_centiseconds / 6_000) % 60;
    let seconds = (total_centiseconds / 100) % 60;
    let centiseconds = total_centiseconds % 100;
    format!("{hours}:{minutes:02}:{seconds:02}.{centiseconds:02}")
}

fn escape_ass_text(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('{', r"\{")
        .replace('}', r"\}")
        .replace(['\r', '\n'], r"\N")
}

fn escape_ass_field(value: &str) -> String {
    escape_ass_text(&value.replace(',', " "))
}

fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', r"\\")
        .replace(':', r"\:")
        .replace('\'', r"\'")
        .replace('[', r"\[")
        .replace(']', r"\]")
}

fn ffmpeg_time(value: Microseconds) -> String {
    format!("{}.{:06}", value.0 / 1_000_000, value.0 % 1_000_000)
}

fn profile_dimensions(profile: RenderProfile, layout: &LayoutPlan) -> (u32, u32) {
    if matches!(layout.mode, CanvasMode::Custom) {
        return (layout.canvas.width, layout.canvas.height);
    }
    match (profile, &layout.mode) {
        (RenderProfile::Proxy, CanvasMode::Portrait) => (540, 960),
        (RenderProfile::Preview, CanvasMode::Portrait) => (720, 1280),
        (RenderProfile::Final, CanvasMode::Portrait) => (1080, 1920),
        (RenderProfile::Proxy, CanvasMode::Square) => (540, 540),
        (RenderProfile::Preview, CanvasMode::Square) => (720, 720),
        (RenderProfile::Final, CanvasMode::Square) => (1080, 1080),
        (RenderProfile::Proxy, _) => (960, 540),
        (RenderProfile::Preview, _) => (1280, 720),
        (RenderProfile::Final, _) => (3840, 2160),
    }
}

fn validate_executable(path: &Path) -> VideoResult<PathBuf> {
    let path = fs::canonicalize(path).map_err(|error| {
        VideoError::new(
            VideoErrorCode::InvalidAsset,
            format!("FFmpeg could not be resolved: {error}"),
        )
    })?;
    let executable = fs::metadata(&path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !executable {
        return Err(VideoError::new(
            VideoErrorCode::InvalidAsset,
            "FFmpeg is not an executable regular file",
        ));
    }
    Ok(path)
}

fn validate_output_path(path: &Path) -> VideoResult<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        VideoError::new(
            VideoErrorCode::InvalidArtifact,
            "render output has no parent directory",
        )
    })?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        VideoError::new(
            VideoErrorCode::InvalidArtifact,
            format!("render output directory could not be opened: {error}"),
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        VideoError::new(
            VideoErrorCode::InvalidArtifact,
            "render output has no filename",
        )
    })?;
    let output = parent.join(name);
    if fs::symlink_metadata(&output).is_ok() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidArtifact,
            "render staging output already exists",
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::contracts::{
        AudioMix, AudioMixTrack, CanvasSpec, GapReason, LayoutPlan, MediaProbe, MediaReference,
        NormalizedRect, Provenance, ProvenanceKind, RationalFrameRate, RationalRate, ReviewState,
        ReviewedScene, SourceAsset, SourceAssetKind, TimeRange, TimelineClip, TimelineGap,
        TimelineTrack, TrackKind,
    };
    use super::*;
    use std::process::{Command, Stdio};

    fn timestamp() -> String {
        "2026-08-27T20:00:00.000Z".into()
    }

    fn mean_volume_db(ffmpeg: &Path, media: &Path, start: &str, duration: &str) -> f64 {
        let result = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "info",
                "-ss",
                start,
                "-t",
                duration,
                "-i",
            ])
            .arg(media)
            .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8_lossy(&result.stderr)
            .lines()
            .find_map(|line| {
                let value = line.split("mean_volume:").nth(1)?.trim();
                let number = value.split_whitespace().next()?;
                if number == "-inf" {
                    Some(f64::NEG_INFINITY)
                } else {
                    number.parse().ok()
                }
            })
            .expect("FFmpeg volumedetect emitted mean_volume")
    }

    fn fixture_manifest(source_path: &Path) -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "assembly-project",
            "Assembly fixture",
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
                background_rgba: [24, 24, 24, 255],
                elements: vec![],
            },
            AudioMix {
                target_lufs_milli: -14_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            timestamp(),
        )
        .unwrap();
        manifest.source_assets.push(SourceAsset {
            id: "source-1".into(),
            kind: SourceAssetKind::LocalVideo,
            managed_path: source_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            sha256: "a".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(1_000_000),
                width: Some(320),
                height: Some(180),
                frame_rate: Some(RationalFrameRate::FPS_30),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: timestamp(),
                producer: "assembly-test".into(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        manifest.reviewed_scenes.push(ReviewedScene {
            id: "scene-1".into(),
            candidate_id: None,
            source_asset_id: Some("source-1".into()),
            source_range: Some(TimeRange::new(0, 1_000_000).unwrap()),
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(1_000_000),
            title: "A clear opening".into(),
            script: "A clear opening matters.".into(),
            review_state: ReviewState::Approved,
            revision: 1,
        });
        let make_clip = |id: &str| TimelineClip {
            id: id.into(),
            scene_id: Some("scene-1".into()),
            media: MediaReference {
                source_asset_id: Some("source-1".into()),
                render_artifact_id: None,
            },
            source_range: TimeRange::new(0, 1_000_000).unwrap(),
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(1_000_000),
            playback_rate: RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        };
        manifest.tracks = vec![
            TimelineTrack {
                id: "video-main".into(),
                kind: TrackKind::Video,
                clips: vec![make_clip("video-clip")],
                preserve_gaps: true,
            },
            TimelineTrack {
                id: "audio-main".into(),
                kind: TrackKind::Audio,
                clips: vec![make_clip("audio-clip")],
                preserve_gaps: true,
            },
        ];
        manifest.audio_mix.tracks.push(AudioMixTrack {
            track_id: "audio-main".into(),
            gain_db_milli: 0,
            pan_milli: 0,
            ducking: None,
        });
        manifest.captions.push(CaptionCue {
            id: "caption-1".into(),
            range: TimeRange::new(100_000, 900_000).unwrap(),
            text: "A clear {opening}, safely.".into(),
            style_id: "clean-white".into(),
            speaker_id: Some("Host".into()),
            transcript_segment_id: None,
            scene_id: Some("scene-1".into()),
        });
        manifest.validate_strict().unwrap();
        manifest
    }

    fn canonical_timeline_fixture(source_path: &Path) -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "canonical-assembly-project",
            "Canonical assembly fixture",
            RationalFrameRate::FPS_30,
            Microseconds(2_250_000),
            LayoutPlan {
                mode: CanvasMode::Custom,
                canvas: CanvasSpec {
                    width: 320,
                    height: 240,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 9_000,
                    height_bp: 9_000,
                },
                background_rgba: [16, 32, 48, 255],
                elements: vec![],
            },
            AudioMix {
                target_lufs_milli: -14_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            timestamp(),
        )
        .unwrap();
        manifest.source_assets.push(SourceAsset {
            id: "source-canonical".into(),
            kind: SourceAssetKind::LocalVideo,
            managed_path: source_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            sha256: "d".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(3_000_000),
                width: Some(640),
                height: Some(360),
                frame_rate: Some(RationalFrameRate::FPS_30),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: timestamp(),
                producer: "assembly-test".into(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        manifest.reviewed_scenes = vec![
            ReviewedScene {
                id: "scene-fast".into(),
                candidate_id: None,
                source_asset_id: Some("source-canonical".into()),
                source_range: Some(TimeRange::new(0, 2_000_000).unwrap()),
                timeline_start_us: Microseconds::ZERO,
                timeline_duration_us: Microseconds(1_000_000),
                title: "Fast cropped opening".into(),
                script: "Metadata for the title card only.".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
            ReviewedScene {
                id: "scene-normal".into(),
                candidate_id: None,
                source_asset_id: Some("source-canonical".into()),
                source_range: Some(TimeRange::new(2_000_000, 3_000_000).unwrap()),
                timeline_start_us: Microseconds(1_250_000),
                timeline_duration_us: Microseconds(1_000_000),
                title: "Normal close".into(),
                script: "Second title metadata.".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
        ];
        let make_clip = |id: &str,
                         scene_id: &str,
                         source_start: i64,
                         source_end: i64,
                         timeline_start: i64,
                         timeline_duration: i64,
                         playback_rate: RationalRate,
                         gain_db_milli: i32,
                         crop: Option<NormalizedRect>| TimelineClip {
            id: id.into(),
            scene_id: Some(scene_id.into()),
            media: MediaReference {
                source_asset_id: Some("source-canonical".into()),
                render_artifact_id: None,
            },
            source_range: TimeRange::new(source_start, source_end).unwrap(),
            timeline_start_us: Microseconds(timeline_start),
            timeline_duration_us: Microseconds(timeline_duration),
            playback_rate,
            gain_db_milli,
            muted: false,
            crop,
        };
        let fast_rate = RationalRate {
            numerator: 2,
            denominator: 1,
        };
        let center_crop = Some(NormalizedRect {
            x_bp: 2_500,
            y_bp: 0,
            width_bp: 5_000,
            height_bp: 10_000,
        });
        manifest.tracks = vec![
            TimelineTrack {
                id: "video-main".into(),
                kind: TrackKind::Video,
                clips: vec![
                    make_clip(
                        "video-fast",
                        "scene-fast",
                        0,
                        2_000_000,
                        0,
                        1_000_000,
                        fast_rate,
                        0,
                        center_crop,
                    ),
                    make_clip(
                        "video-normal",
                        "scene-normal",
                        2_000_000,
                        3_000_000,
                        1_250_000,
                        1_000_000,
                        RationalRate::ONE,
                        0,
                        None,
                    ),
                ],
                preserve_gaps: true,
            },
            TimelineTrack {
                id: "audio-main".into(),
                kind: TrackKind::Audio,
                clips: vec![
                    make_clip(
                        "audio-fast",
                        "scene-fast",
                        0,
                        2_000_000,
                        0,
                        1_000_000,
                        fast_rate,
                        -6_000,
                        None,
                    ),
                    make_clip(
                        "audio-normal",
                        "scene-normal",
                        2_000_000,
                        3_000_000,
                        1_250_000,
                        1_000_000,
                        RationalRate::ONE,
                        0,
                        None,
                    ),
                ],
                preserve_gaps: true,
            },
        ];
        manifest.gaps = vec![
            TimelineGap {
                id: "gap-video".into(),
                track_id: "video-main".into(),
                range: TimeRange::new(1_000_000, 1_250_000).unwrap(),
                reason: GapReason::Transition,
                source_asset_id: None,
                source_range: None,
            },
            TimelineGap {
                id: "gap-audio".into(),
                track_id: "audio-main".into(),
                range: TimeRange::new(1_000_000, 1_250_000).unwrap(),
                reason: GapReason::Editorial,
                source_asset_id: None,
                source_range: None,
            },
        ];
        manifest.audio_mix.tracks.push(AudioMixTrack {
            track_id: "audio-main".into(),
            gain_db_milli: -3_000,
            pan_milli: 0,
            ducking: None,
        });
        manifest.validate_strict().unwrap();
        manifest
    }

    #[test]
    fn ass_document_escapes_text_and_uses_timeline_clock() {
        let root = std::env::temp_dir().join(format!("soundar-ass-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let manifest = fixture_manifest(&source);
        let document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();
        assert!(document.contains("0:00:00.10"));
        assert!(document.contains(r"\{opening\}"));
        assert!(document.contains("Style: Speaker"));
        assert!(document.contains("0:00:00.10,0:00:00.90,Speaker"));
        let published = write_ass_document_atomic(&root.join("captions.ass"), &document).unwrap();
        assert_eq!(fs::read_to_string(published).unwrap(), document);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn assembly_plan_is_shell_free_and_has_software_fallback() {
        if !Path::new("/usr/bin/ffmpeg").is_file() {
            return;
        }
        let root = std::env::temp_dir().join(format!("soundar-plan-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let manifest = fixture_manifest(&source);
        let sources = BTreeMap::from([("source-1".into(), source)]);
        let plan = build_timeline_render_plan(
            Path::new("/usr/bin/ffmpeg"),
            &manifest,
            &sources,
            None,
            &root.join("output.mp4"),
            &AssemblyOptions::default(),
            true,
        )
        .unwrap();
        assert_eq!(plan.primary.encoder, VideoEncoder::H264Nvenc);
        assert_eq!(
            plan.software_fallback.as_ref().unwrap().encoder,
            VideoEncoder::Libx264
        );
        assert!(!plan
            .primary
            .args
            .iter()
            .any(|arg| arg == "sh" || arg == "-c"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_silent_audio_track_does_not_fall_back_to_embedded_source_audio() {
        if !Path::new("/usr/bin/ffmpeg").is_file() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("soundar-silent-track-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.tracks[1].clips.clear();
        manifest.gaps.push(TimelineGap {
            id: "gap-audio-full".into(),
            track_id: "audio-main".into(),
            range: TimeRange::new(0, 1_000_000).unwrap(),
            reason: GapReason::Editorial,
            source_asset_id: None,
            source_range: None,
        });
        manifest.validate_strict().unwrap();
        let plan = build_timeline_render_plan(
            Path::new("/usr/bin/ffmpeg"),
            &manifest,
            &BTreeMap::from([("source-1".into(), source)]),
            None,
            &root.join("output.mp4"),
            &AssemblyOptions::default(),
            false,
        )
        .unwrap();
        let filter = plan
            .primary
            .args
            .windows(2)
            .find(|pair| pair[0] == "-filter_complex")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .unwrap();
        assert!(!filter.contains(":a:0]"));
        assert!(filter.contains("anullsrc=r=48000:cl=stereo"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn audio_only_canonical_timeline_keeps_the_podcast_waveform_visual() {
        if !Path::new("/usr/bin/ffmpeg").is_file() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("soundar-waveform-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.tracks.remove(0);
        manifest.validate_strict().unwrap();
        let plan = build_timeline_render_plan(
            Path::new("/usr/bin/ffmpeg"),
            &manifest,
            &BTreeMap::from([("source-1".into(), source)]),
            None,
            &root.join("output.mp4"),
            &AssemblyOptions::default(),
            false,
        )
        .unwrap();
        let filter = plan
            .primary
            .args
            .windows(2)
            .find(|pair| pair[0] == "-filter_complex")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .unwrap();
        assert!(filter.contains("showwaves="));
        assert!(filter.contains("asplit=2[waveform_audio][audio_for_master]"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn real_ffmpeg_assembles_playable_portrait_fixture_with_captions() {
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let ffprobe = Path::new("/usr/bin/ffprobe");
        if !ffmpeg.is_file() || !ffprobe.is_file() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("soundar-assembly-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let fixture_status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x334155:s=320x180:r=30:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(fixture_status.success());
        let manifest = fixture_manifest(&source);
        let options = AssemblyOptions {
            profile: RenderProfile::Proxy,
            portrait_layout: PortraitLayout::Contain,
            caption_theme: CaptionTheme::Calm,
            ..AssemblyOptions::default()
        };
        let captions_path = root.join("captions.ass");
        write_ass_document_atomic(
            &captions_path,
            &build_ass_document(&manifest, &options).unwrap(),
        )
        .unwrap();
        let output = root.join("assembled.mp4");
        let plan = build_timeline_render_plan(
            ffmpeg,
            &manifest,
            &BTreeMap::from([("source-1".into(), source)]),
            Some(&captions_path),
            &output,
            &options,
            false,
        )
        .unwrap();
        let status = plan
            .primary
            .command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let probe = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,width,height",
                "-of",
                "json",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(probe.status.success());
        let value: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        assert!(value["streams"]
            .as_array()
            .is_some_and(
                |streams| streams.iter().any(|stream| stream["codec_type"] == "video"
                    && stream["width"] == 540
                    && stream["height"] == 960)
            ));
        assert!(value["streams"]
            .as_array()
            .is_some_and(|streams| streams.iter().any(|stream| stream["codec_type"] == "audio")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn real_ffmpeg_renders_canonical_tracks_gap_crop_rate_gain_and_custom_canvas() {
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let ffprobe = Path::new("/usr/bin/ffprobe");
        if !ffmpeg.is_file() || !ffprobe.is_file() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "soundar-canonical-assembly-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let fixture_status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x360:r=30:d=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=3",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                "-y",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(fixture_status.success());

        let manifest = canonical_timeline_fixture(&source);
        let output = root.join("canonical.mp4");
        let options = AssemblyOptions {
            profile: RenderProfile::Proxy,
            portrait_layout: PortraitLayout::Contain,
            burn_captions: false,
            ..AssemblyOptions::default()
        };
        let plan = build_timeline_render_plan(
            ffmpeg,
            &manifest,
            &BTreeMap::from([("source:source-canonical".into(), source)]),
            None,
            &output,
            &options,
            false,
        )
        .unwrap();
        let filter = plan
            .primary
            .args
            .windows(2)
            .find(|pair| pair[0] == "-filter_complex")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .unwrap();
        assert!(filter.contains("setpts=(PTS-STARTPTS)*1/2"));
        assert!(filter.contains("crop=w='max(2\\,trunc(iw*5000/20000)*2)'"));
        assert!(filter.contains("d=0.250000"));
        assert!(filter.contains("volume=-6.000dB"));
        assert!(filter.contains("volume=-3.000dB"));
        assert!(filter.contains("pad=320:240"));

        let status = plan
            .primary
            .command()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let probe = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration:stream=codec_type,width,height",
                "-of",
                "json",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(probe.status.success());
        let value: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let streams = value["streams"].as_array().unwrap();
        assert!(streams.iter().any(|stream| {
            stream["codec_type"] == "video" && stream["width"] == 320 && stream["height"] == 240
        }));
        assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
        let duration = value["format"]["duration"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap();
        assert!((2.20..=2.35).contains(&duration), "duration={duration}");

        let first_volume = mean_volume_db(ffmpeg, &output, "0.10", "0.70");
        let second_volume = mean_volume_db(ffmpeg, &output, "1.35", "0.70");
        let gap_volume = mean_volume_db(ffmpeg, &output, "1.05", "0.10");
        assert!(
            second_volume - first_volume >= 4.5,
            "expected the -6 dB clip gain to remain audible: first={first_volume}, second={second_volume}"
        );
        assert!(
            gap_volume < -60.0,
            "explicit audio gap was not silent: {gap_volume} dB"
        );

        let silence = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "info", "-i"])
            .arg(&output)
            .args([
                "-vn",
                "-af",
                "silencedetect=noise=-50dB:d=0.10",
                "-f",
                "null",
                "-",
            ])
            .output()
            .unwrap();
        assert!(silence.status.success());
        let diagnostics = String::from_utf8_lossy(&silence.stderr);
        assert!(diagnostics.contains("silence_start"), "{diagnostics}");
        assert!(diagnostics.contains("silence_end"), "{diagnostics}");
        fs::remove_dir_all(root).ok();
    }
}
