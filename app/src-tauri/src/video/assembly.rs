//! FFmpeg assembly planning for reviewed Video Studio timelines.
//!
//! Media timing is read exclusively from canonical timeline tracks. Each clip is sought from its
//! original source clock, deliberate timeline holes become explicit black/silent segments, and
//! scene metadata is reserved for title cards. Captions and speaker cards are burned from a
//! generated ASS document. Commands never involve a shell, and both NVENC and software plans are
//! deterministic.

use super::contracts::{
    caption_bounds_for_scene, AudioMixTrack, CanvasMode, CaptionCue, CaptionPresetId, LayoutPlan,
    LayoutRole, Microseconds, NormalizedRect, RationalRate, RenderArtifact, SourceAsset, TimeRange,
    TimelineClip, TimelineTrack, TrackKind, VideoError, VideoErrorCode, VideoProjectManifest,
    VideoResult, NORMALIZED_BASIS_POINTS,
};
use super::media::local_media_input_args;
use super::renderer::{
    PortraitLayout, RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass,
    VideoEncoder,
};
use super::timeline::{
    map_source_endpoint_to_timeline, partition_track, QuantizeMode, TimelineSpanKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use super::visuals::{VisualEasing, VisualFit, VisualLayer};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionTheme {
    CleanWhite,
    Calm,
    Kinetic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptionRevealMode {
    Page,
    ActiveWord,
    Karaoke,
    Typewriter,
}

#[derive(Clone, Copy, Debug)]
struct CaptionPagingRules {
    max_words: usize,
    max_lines: usize,
    max_chars_per_line: usize,
    preferred_min_page_us: i64,
    break_on_punctuation: bool,
}

#[derive(Clone, Copy, Debug)]
struct AssCaptionPreset {
    id: CaptionPresetId,
    ass_name: &'static str,
    font_name: &'static str,
    relative_size: f64,
    primary_color: &'static str,
    secondary_color: &'static str,
    active_color: &'static str,
    outline_color: &'static str,
    back_color: &'static str,
    bold: bool,
    spacing: i8,
    border_style: u8,
    outline: u8,
    shadow: u8,
    alignment: u8,
    margin_vertical_fraction: f64,
    uppercase: bool,
    lowercase: bool,
    reveal: CaptionRevealMode,
    paging: CaptionPagingRules,
}

#[derive(Clone, Debug)]
struct TimedCaptionToken {
    text: String,
    space_before: bool,
    start_us: Microseconds,
    end_us: Microseconds,
}

#[derive(Clone, Debug)]
struct CaptionPage {
    tokens: Vec<TimedCaptionToken>,
    start_us: Microseconds,
    end_us: Microseconds,
}

/// Authoritative caption page projection for live preview. The UI must render these pages rather
/// than repeat word-count paging: the same private plan feeds ASS preview and final export.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaptionPreviewPage {
    pub id: String,
    pub cue_id: String,
    pub scene_id: Option<String>,
    pub start_us: Microseconds,
    pub end_us: Microseconds,
    pub text: String,
    pub style_id: String,
    pub bounds: NormalizedRect,
    /// Font size normalized to canvas height (10_000 == full canvas height).
    pub font_size_bp: u16,
    pub words: Vec<CaptionPreviewWord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaptionPreviewWord {
    pub text: String,
    pub start_us: Microseconds,
    pub end_us: Microseconds,
}

const MIN_ASS_PAGE_DURATION_US: i64 = 20_000;

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
    let mut visual_input_by_asset = BTreeMap::new();
    for layer in &manifest.visual_layers {
        if visual_input_by_asset.contains_key(layer.asset_id.as_str()) {
            continue;
        }
        let asset = manifest
            .visual_assets
            .iter()
            .find(|asset| asset.id == layer.asset_id)
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::MissingReference,
                    format!("visual layer {} has no managed asset", layer.id),
                )
            })?;
        let namespaced_id = format!("visual:{}", asset.id);
        let path = resolved_sources
            .get(&namespaced_id)
            .or_else(|| resolved_sources.get(asset.id.as_str()))
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::MissingReference,
                    format!("visual asset {} has no resolved managed file", asset.id),
                )
            })?;
        let path = fs::canonicalize(path).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidAsset,
                format!("visual asset {} could not be opened: {error}", asset.id),
            )
        })?;
        let input_index = input_by_clip.len() + visual_input_by_asset.len();
        visual_input_by_asset.insert(asset.id.as_str(), input_index);
        args.extend([
            OsString::from("-loop"),
            OsString::from("1"),
            OsString::from("-framerate"),
            OsString::from(&frame_rate),
        ]);
        args.extend(local_media_input_args(&path).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidAsset,
                format!(
                    "visual asset {} failed local-input policy: {}",
                    asset.id, error.message
                ),
            )
        })?);
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
    if !manifest.visual_layers.is_empty() {
        assembled_video = append_visual_layer_filters(
            &mut filters,
            &assembled_video,
            &manifest.visual_layers,
            &visual_input_by_asset,
            width,
            height,
        )?;
    }

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
            "[video_output]".to_string()
        } else {
            format!("[{assembled_video}]")
        }
    } else {
        format!("[{assembled_video}]")
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
        OsString::from(&video_output),
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

fn append_visual_layer_filters(
    filters: &mut Vec<String>,
    base_label: &str,
    layers: &[VisualLayer],
    input_by_asset: &BTreeMap<&str, usize>,
    canvas_width: u32,
    canvas_height: u32,
) -> VideoResult<String> {
    let mut ordered = layers.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (left.z_index, left.range.start_us, &left.id).cmp(&(
            right.z_index,
            right.range.start_us,
            &right.id,
        ))
    });
    let mut composed = base_label.to_string();
    for (index, layer) in ordered.into_iter().enumerate() {
        let input = input_by_asset.get(layer.asset_id.as_str()).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::MissingReference,
                format!("visual layer {} has no FFmpeg input", layer.id),
            )
        })?;
        let duration_us = layer.range.duration()?.0;
        let start_seconds = ffmpeg_time(layer.range.start_us);
        let end_seconds = ffmpeg_time(layer.range.end_us);
        let duration_seconds = ffmpeg_time(Microseconds(duration_us));
        let start_width = basis_pixels(layer.motion.start_bounds.width_bp, canvas_width);
        let start_height = basis_pixels(layer.motion.start_bounds.height_bp, canvas_height);
        let end_width = basis_pixels(layer.motion.end_bounds.width_bp, canvas_width);
        let end_height = basis_pixels(layer.motion.end_bounds.height_bp, canvas_height);
        let start_x = basis_pixels_signed(layer.motion.start_bounds.x_bp, canvas_width);
        let start_y = basis_pixels_signed(layer.motion.start_bounds.y_bp, canvas_height);
        let end_x = basis_pixels_signed(layer.motion.end_bounds.x_bp, canvas_width);
        let end_y = basis_pixels_signed(layer.motion.end_bounds.y_bp, canvas_height);
        let progress =
            visual_progress_expression(layer.motion.easing, &start_seconds, &duration_seconds);
        let width_expression = visual_interpolation(start_width, end_width, &progress, true);
        let height_expression = visual_interpolation(start_height, end_height, &progress, true);
        let x_expression = visual_interpolation(start_x, end_x, &progress, false);
        let y_expression = visual_interpolation(start_y, end_y, &progress, false);
        let source_label = format!("visual_source_{index}");
        let animated_label = format!("visual_animated_{index}");
        let output_label = format!("visual_composite_{index}");
        let mut source = format!("[{input}:v:0]format=rgba");
        if let Some(crop) = layer.crop {
            source.push(',');
            source.push_str(&normalized_crop_filter(crop));
        }
        match layer.fit {
            VisualFit::Stretch => source.push_str(&format!(
                ",scale={start_width}:{start_height}:flags=lanczos"
            )),
            VisualFit::Cover => source.push_str(&format!(
                ",scale=w={start_width}:h={start_height}:force_original_aspect_ratio=increase:flags=lanczos,crop={start_width}:{start_height}"
            )),
            VisualFit::Contain => source.push_str(&format!(
                ",scale=w={start_width}:h={start_height}:force_original_aspect_ratio=decrease:flags=lanczos,pad={start_width}:{start_height}:(ow-iw)/2:(oh-ih)/2:color=black@0"
            )),
        }
        source.push_str(&format!(
            ",setsar=1,trim=duration={duration_seconds},setpts=PTS-STARTPTS+{start_seconds}/TB[{source_label}]"
        ));
        filters.push(source);

        let opacity = f64::from(layer.motion.start_opacity_milli) / 1_000.0;
        let mut animation = format!(
            "[{source_label}]scale=w='{width_expression}':h='{height_expression}':eval=frame:flags=lanczos,format=rgba,colorchannelmixer=aa={opacity:.3}"
        );
        if layer.transition_in_us.0 > 0 {
            animation.push_str(&format!(
                ",fade=t=in:st={start_seconds}:d={}:alpha=1",
                ffmpeg_time(layer.transition_in_us)
            ));
        }
        if layer.transition_out_us.0 > 0 {
            let fade_start = layer
                .range
                .end_us
                .checked_add(Microseconds(-layer.transition_out_us.0))?;
            animation.push_str(&format!(
                ",fade=t=out:st={}:d={}:alpha=1",
                ffmpeg_time(fade_start),
                ffmpeg_time(layer.transition_out_us)
            ));
        }
        animation.push_str(&format!("[{animated_label}]"));
        filters.push(animation);
        filters.push(format!(
            "[{composed}][{animated_label}]overlay=x='{x_expression}':y='{y_expression}':enable='between(t,{start_seconds},{end_seconds})':eof_action=pass:shortest=0[{output_label}]"
        ));
        composed = output_label;
    }
    Ok(composed)
}

fn visual_progress_expression(
    easing: VisualEasing,
    start_seconds: &str,
    duration_seconds: &str,
) -> String {
    let linear = format!("min(max((t-{start_seconds})/{duration_seconds}\\,0)\\,1)");
    match easing {
        VisualEasing::Linear => linear,
        VisualEasing::EaseInOut => format!("({linear})*({linear})*(3-2*({linear}))"),
    }
}

fn visual_interpolation(start: i64, end: i64, progress: &str, even: bool) -> String {
    let value = format!("({start}+({end}-{start})*({progress}))");
    if even {
        format!("max(2,trunc(({value})/2)*2)")
    } else {
        value
    }
}

fn basis_pixels(value_bp: i32, dimension: u32) -> i64 {
    ((i64::from(value_bp) * i64::from(dimension) + 5_000) / 10_000).max(2)
}

fn basis_pixels_signed(value_bp: i32, dimension: u32) -> i64 {
    (i64::from(value_bp) * i64::from(dimension) + 5_000) / 10_000
}

/// Generate the caption/title/speaker overlay document from timeline-clock cues. The document is
/// deterministic for a manifest revision and can be content-addressed independently of video.
pub fn build_ass_document(
    manifest: &VideoProjectManifest,
    options: &AssemblyOptions,
) -> VideoResult<String> {
    manifest.validate_strict()?;
    let (width, height) = profile_dimensions(options.profile, &manifest.layout);
    // `caption_theme` remains serialized in render requests for backwards compatibility and
    // variation provenance. A cue's validated style_id is now authoritative, which lets one
    // timeline render different scene looks without a shadow global caption clock or theme.
    let _legacy_theme = options.caption_theme;
    let mut document = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nScaledBorderAndShadow: yes\nWrapStyle: 0\n; CaptionTiming: canonical timeline clock; exact source words or bounded in-cue fallback\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
    );
    for preset_id in CaptionPresetId::ALL {
        document.push_str(&ass_caption_style(caption_preset(preset_id), width, height));
    }
    let clean_font_size = caption_font_size(caption_preset(CaptionPresetId::CleanWhite), height);
    document.push_str(&format!(
        "Style: Title,Inter,{},&H00FFFFFF,&H00FFFFFF,&H90000000,&H50000000,-1,0,0,0,100,100,0,0,1,3,0,8,90,90,{},1\nStyle: Speaker,Inter,{},&H00FFFFFF,&H00FFFFFF,&H80000000,&H50000000,-1,0,0,0,100,100,0,0,3,1,0,7,70,70,70,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        (clean_font_size as f64 * 1.08).round() as u32,
        (height as f64 * 0.07).round() as u32,
        (clean_font_size as f64 * 0.68).round() as u32,
    ));
    let mut ordered_captions = manifest.captions.iter().collect::<Vec<_>>();
    ordered_captions.sort_by(|left, right| {
        (left.range.start_us, left.range.end_us, left.id.as_str()).cmp(&(
            right.range.start_us,
            right.range.end_us,
            right.id.as_str(),
        ))
    });
    for caption in ordered_captions {
        append_caption_events(&mut document, manifest, caption, width, height)?;
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
        // A performed script knows who is speaking from its own takes, which is a better source
        // than captions that may not exist for it.
        if manifest.dialogue.is_empty() {
            append_speaker_cards(&mut document, &manifest.captions)?;
        } else {
            append_dialogue_speaker_cards(&mut document, manifest)?;
        }
    }
    Ok(document)
}

/// Name the character speaking each time the voice changes, from the takes as placed.
///
/// A card is shown when a new character starts a line, and held for the first moment of it: long
/// enough to read, short enough to leave the frame to the picture. Consecutive lines by one
/// character get one card.
fn append_dialogue_speaker_cards(
    document: &mut String,
    manifest: &VideoProjectManifest,
) -> VideoResult<()> {
    let names = manifest
        .cast
        .iter()
        .map(|member| (member.id.as_str(), member.display_name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let turns = manifest
        .dialogue
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect::<BTreeMap<_, _>>();
    let mut placed = manifest
        .tracks
        .iter()
        .filter(|track| matches!(track.kind, TrackKind::Audio))
        .flat_map(|track| track.clips.iter())
        .filter_map(|clip| {
            let turn = turns.get(clip.turn_id.as_deref()?)?;
            Some((
                clip.timeline_start_us,
                clip.timeline_duration_us,
                turn.character_id.as_str(),
            ))
        })
        .collect::<Vec<_>>();
    placed.sort_by_key(|(start, duration, _)| (*start, *duration));
    let mut prior: Option<&str> = None;
    for (start, duration, character_id) in placed {
        if prior == Some(character_id) {
            continue;
        }
        let name = names
            .get(character_id)
            .copied()
            .unwrap_or(character_id)
            .trim();
        if name.is_empty() {
            continue;
        }
        let end = start
            .checked_add(Microseconds(1_800_000))?
            .min(start.checked_add(duration)?);
        if end <= start {
            continue;
        }
        document.push_str(&ass_dialogue(
            30,
            start,
            end,
            "Speaker",
            name,
            &format!("{{\\fad(80,180)}}{}", escape_ass_text(name)),
        ));
        prior = Some(character_id);
    }
    Ok(())
}

pub fn plan_caption_preview_pages(
    manifest: &VideoProjectManifest,
) -> VideoResult<Vec<CaptionPreviewPage>> {
    manifest.validate_strict()?;
    let mut projected = Vec::new();
    for caption in &manifest.captions {
        let (preset, pages) = planned_caption_pages_for_cue(manifest, caption)?;
        for (index, page) in pages.into_iter().enumerate() {
            let geometry = caption_geometry_for_page(manifest, caption, preset, &page)?;
            let id = format!("{}-page-{:03}", caption.id, index + 1);
            let words = page
                .tokens
                .iter()
                .filter_map(|token| {
                    let start_us = token.start_us.max(page.start_us);
                    let end_us = token.end_us.min(page.end_us);
                    (end_us > start_us).then(|| CaptionPreviewWord {
                        text: token.text.clone(),
                        start_us,
                        end_us,
                    })
                })
                .collect();
            projected.push(CaptionPreviewPage {
                id,
                cue_id: caption.id.clone(),
                scene_id: caption.scene_id.clone(),
                start_us: page.start_us,
                end_us: page.end_us,
                text: plain_caption_page_text(&page, preset),
                style_id: preset.id.public_id().to_string(),
                bounds: geometry.bounds,
                font_size_bp: geometry.font_size_bp,
                words,
            });
        }
    }
    projected.sort_by(|left, right| {
        (
            left.start_us,
            left.end_us,
            left.cue_id.as_str(),
            left.id.as_str(),
        )
            .cmp(&(
                right.start_us,
                right.end_us,
                right.cue_id.as_str(),
                right.id.as_str(),
            ))
    });
    Ok(projected)
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
        VideoEncoder::Image | VideoEncoder::AudioOnly => {}
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
    let mut ordered = captions.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (left.range.start_us, left.range.end_us, left.id.as_str()).cmp(&(
            right.range.start_us,
            right.range.end_us,
            right.id.as_str(),
        ))
    });
    for caption in ordered {
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

/// Project every caption preset into a web-renderable spec.
///
/// The editor preview used to approximate these presets with hand-written CSS, which drifted from
/// what the renderer burns in: presets grew pill backgrounds the export never drew, and the words
/// themselves never changed weight, family, or casing. Deriving the preview from this table keeps
/// the canvas honest — what the user picks is what FFmpeg renders.
pub fn present_caption_presets() -> Vec<Value> {
    CaptionPresetId::ALL
        .iter()
        .map(|id| {
            let preset = caption_preset(*id);
            json!({
                "id": preset.id.public_id(),
                "label": caption_preset_label(preset.id),
                "font_family": web_font_stack(preset.font_name),
                // Fraction of canvas height, matching the renderer's own sizing.
                "relative_size": preset.relative_size,
                "text_color": ass_color_to_css(preset.primary_color),
                "active_color": ass_color_to_css(preset.active_color),
                "outline_color": ass_color_to_css(preset.outline_color),
                // libass paints the back colour only in opaque-box mode; anything else has no box.
                "background_color": if preset.border_style == 3 { ass_color_to_css(preset.back_color) } else { Value::Null },
                "bold": preset.bold,
                "letter_spacing_em": f64::from(preset.spacing) * 0.02,
                "outline_em": f64::from(preset.outline) * 0.02,
                "casing": if preset.uppercase { "upper" } else if preset.lowercase { "lower" } else { "as-is" },
                "reveal": match preset.reveal {
                    CaptionRevealMode::Page => "page",
                    CaptionRevealMode::ActiveWord => "active-word",
                    CaptionRevealMode::Karaoke => "karaoke",
                    CaptionRevealMode::Typewriter => "typewriter",
                },
                "max_words_per_page": preset.paging.max_words,
                "max_lines": preset.paging.max_lines,
            })
        })
        .collect()
}

fn caption_preset_label(id: CaptionPresetId) -> &'static str {
    match id {
        CaptionPresetId::CleanWhite => "Clean",
        CaptionPresetId::Calm => "Calm",
        CaptionPresetId::Kinetic => "Kinetic",
        CaptionPresetId::BoldPop => "Bold pop",
        CaptionPresetId::Highlight => "Highlight",
        CaptionPresetId::Karaoke => "Karaoke",
        CaptionPresetId::Typewriter => "Typewriter",
        CaptionPresetId::Podcast => "Podcast",
    }
}

/// The renderer names one font; the webview needs a stack that resolves to the same shape.
fn web_font_stack(font_name: &str) -> &'static str {
    match font_name {
        "DejaVu Sans Mono" => "\"JetBrains Mono Variable\", \"DejaVu Sans Mono\", monospace",
        _ => "\"Inter Variable\", Inter, system-ui, sans-serif",
    }
}

/// Convert an ASS `&HAABBGGRR` literal to a CSS colour.
///
/// ASS stores the channels reversed and treats alpha as transparency, so `00` is fully opaque.
fn ass_color_to_css(value: &str) -> Value {
    let digits = value.trim_start_matches("&H").trim_end_matches('&');
    let Ok(raw) = u32::from_str_radix(digits, 16) else {
        return Value::Null;
    };
    let (alpha, blue, green, red) = (
        (raw >> 24) & 0xFF,
        (raw >> 16) & 0xFF,
        (raw >> 8) & 0xFF,
        raw & 0xFF,
    );
    let opacity = f64::from(255 - alpha) / 255.0;
    Value::String(format!(
        "rgba({red}, {green}, {blue}, {:.3})",
        (opacity * 1000.0).round() / 1000.0
    ))
}

fn caption_preset(id: CaptionPresetId) -> AssCaptionPreset {
    match id {
        CaptionPresetId::CleanWhite => AssCaptionPreset {
            id,
            ass_name: "CaptionCleanWhite",
            font_name: "Inter",
            relative_size: 0.044,
            primary_color: "&H00FFFFFF",
            secondary_color: "&H00FFFFFF",
            active_color: "&H00FFFFFF",
            outline_color: "&H78000000",
            back_color: "&H3C000000",
            bold: true,
            spacing: 0,
            border_style: 1,
            outline: 3,
            shadow: 0,
            alignment: 2,
            margin_vertical_fraction: 0.09,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::Page,
            paging: CaptionPagingRules {
                max_words: 8,
                max_lines: 2,
                max_chars_per_line: 30,
                preferred_min_page_us: 500_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Calm => AssCaptionPreset {
            id,
            ass_name: "CaptionCalm",
            font_name: "Inter",
            relative_size: 0.036,
            primary_color: "&H00F7F3EE",
            secondary_color: "&H00F7F3EE",
            active_color: "&H00F7F3EE",
            outline_color: "&H8A111111",
            back_color: "&H50000000",
            bold: true,
            spacing: 0,
            border_style: 1,
            outline: 2,
            shadow: 1,
            alignment: 2,
            margin_vertical_fraction: 0.105,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::Page,
            paging: CaptionPagingRules {
                max_words: 10,
                max_lines: 2,
                max_chars_per_line: 34,
                preferred_min_page_us: 650_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Kinetic => AssCaptionPreset {
            id,
            ass_name: "CaptionKinetic",
            font_name: "Inter",
            relative_size: 0.052,
            primary_color: "&H00FFFFFF",
            secondary_color: "&H003DD9FF",
            active_color: "&H003DD9FF",
            outline_color: "&H00000000",
            back_color: "&H50000000",
            bold: true,
            spacing: 1,
            border_style: 1,
            outline: 4,
            shadow: 1,
            alignment: 2,
            margin_vertical_fraction: 0.18,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::Page,
            paging: CaptionPagingRules {
                max_words: 4,
                max_lines: 2,
                max_chars_per_line: 20,
                preferred_min_page_us: 350_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::BoldPop => AssCaptionPreset {
            id,
            ass_name: "CaptionBoldPop",
            font_name: "Inter",
            relative_size: 0.064,
            primary_color: "&H00FFFFFF",
            secondary_color: "&H003DD9FF",
            active_color: "&H003DD9FF",
            outline_color: "&H00000000",
            back_color: "&H60000000",
            bold: true,
            spacing: 1,
            border_style: 1,
            outline: 5,
            shadow: 2,
            alignment: 2,
            margin_vertical_fraction: 0.22,
            uppercase: true,
            lowercase: false,
            reveal: CaptionRevealMode::ActiveWord,
            paging: CaptionPagingRules {
                max_words: 3,
                max_lines: 2,
                max_chars_per_line: 16,
                preferred_min_page_us: 300_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Highlight => AssCaptionPreset {
            id,
            ass_name: "CaptionHighlight",
            font_name: "Inter",
            relative_size: 0.048,
            primary_color: "&H00FFFFFF",
            secondary_color: "&H00F2764F",
            active_color: "&H00F2764F",
            outline_color: "&H70000000",
            back_color: "&H58000000",
            bold: true,
            spacing: 0,
            border_style: 1,
            outline: 3,
            shadow: 1,
            alignment: 2,
            margin_vertical_fraction: 0.13,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::ActiveWord,
            paging: CaptionPagingRules {
                max_words: 5,
                max_lines: 2,
                max_chars_per_line: 24,
                preferred_min_page_us: 400_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Karaoke => AssCaptionPreset {
            id,
            ass_name: "CaptionKaraoke",
            font_name: "Inter",
            relative_size: 0.047,
            primary_color: "&H008C8C8C",
            secondary_color: "&H008C8C8C",
            active_color: "&H00FFFFFF",
            outline_color: "&H78000000",
            back_color: "&H48000000",
            bold: true,
            spacing: 0,
            border_style: 1,
            outline: 3,
            shadow: 0,
            alignment: 2,
            margin_vertical_fraction: 0.11,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::Karaoke,
            paging: CaptionPagingRules {
                max_words: 6,
                max_lines: 2,
                max_chars_per_line: 28,
                preferred_min_page_us: 450_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Typewriter => AssCaptionPreset {
            id,
            ass_name: "CaptionTypewriter",
            font_name: "DejaVu Sans Mono",
            relative_size: 0.038,
            primary_color: "&H006EE7A4",
            secondary_color: "&H006EE7A4",
            active_color: "&H006EE7A4",
            outline_color: "&H50000000",
            back_color: "&H70110D0A",
            bold: false,
            spacing: 1,
            border_style: 3,
            outline: 1,
            shadow: 0,
            alignment: 2,
            margin_vertical_fraction: 0.12,
            uppercase: false,
            lowercase: true,
            reveal: CaptionRevealMode::Typewriter,
            paging: CaptionPagingRules {
                max_words: 7,
                max_lines: 2,
                max_chars_per_line: 30,
                preferred_min_page_us: 400_000,
                break_on_punctuation: true,
            },
        },
        CaptionPresetId::Podcast => AssCaptionPreset {
            id,
            ass_name: "CaptionPodcast",
            font_name: "Inter",
            relative_size: 0.039,
            primary_color: "&H00F3EFE8",
            secondary_color: "&H00F3EFE8",
            active_color: "&H00F3EFE8",
            outline_color: "&H50000000",
            back_color: "&H701A1510",
            bold: true,
            spacing: 0,
            border_style: 3,
            outline: 1,
            shadow: 0,
            alignment: 2,
            margin_vertical_fraction: 0.09,
            uppercase: false,
            lowercase: false,
            reveal: CaptionRevealMode::Page,
            paging: CaptionPagingRules {
                max_words: 10,
                max_lines: 2,
                max_chars_per_line: 34,
                preferred_min_page_us: 650_000,
                break_on_punctuation: true,
            },
        },
    }
}

fn caption_font_size(preset: AssCaptionPreset, height: u32) -> u32 {
    ((height as f64 * preset.relative_size).round() as u32).clamp(22, 112)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaptionRenderGeometry {
    bounds: NormalizedRect,
    font_size_bp: u16,
}

fn caption_geometry_for_page(
    manifest: &VideoProjectManifest,
    caption: &CaptionCue,
    preset: AssCaptionPreset,
    page: &CaptionPage,
) -> VideoResult<CaptionRenderGeometry> {
    let bounds = caption_bounds_for_scene(&manifest.layout, caption.scene_id.as_deref())?;
    let breaks = caption_line_breaks(&page.tokens, preset.paging)
        .expect("caption pages are measured before geometry");
    let mut line_count = 1_u64;
    let mut current_chars = 0_u64;
    let mut maximum_line_chars = 1_u64;
    for (index, token) in page.tokens.iter().enumerate() {
        let token_chars = u64::try_from(token.text.chars().count()).unwrap_or(u64::MAX);
        if index > 0 && breaks[index] {
            maximum_line_chars = maximum_line_chars.max(current_chars);
            current_chars = token_chars;
            line_count = line_count.saturating_add(1);
        } else {
            current_chars = current_chars
                .saturating_add(u64::from(index > 0 && token.space_before))
                .saturating_add(token_chars);
        }
    }
    maximum_line_chars = maximum_line_chars.max(current_chars).max(1);

    let base_bp = (preset.relative_size * f64::from(NORMALIZED_BASIS_POINTS)).round() as u64;
    // Reserve 1.35 em per line (outline/background included), and conservatively budget an
    // average glyph at 0.68 em. These normalized limits are profile-independent, so proxy,
    // preview, final ASS, and the live presentation all agree on the exact relative size.
    let height_cap_bp = u64::try_from(bounds.height_bp)
        .unwrap_or_default()
        .saturating_mul(100)
        / line_count.saturating_mul(135).max(1);
    let width_cap_bp = u64::try_from(bounds.width_bp)
        .unwrap_or_default()
        .saturating_mul(u64::from(manifest.layout.canvas.width))
        .saturating_mul(100)
        / u64::from(manifest.layout.canvas.height)
            .saturating_mul(maximum_line_chars)
            .saturating_mul(68)
            .max(1);
    let font_size_bp = base_bp.min(height_cap_bp).min(width_cap_bp).clamp(1, 1_000) as u16;
    Ok(CaptionRenderGeometry {
        bounds,
        font_size_bp,
    })
}

fn scale_basis_point(value_bp: i32, pixels: u32) -> u32 {
    let value = u64::try_from(value_bp.max(0)).unwrap_or_default();
    ((value.saturating_mul(u64::from(pixels))
        + u64::try_from(NORMALIZED_BASIS_POINTS / 2).unwrap_or_default())
        / u64::try_from(NORMALIZED_BASIS_POINTS).unwrap_or(10_000)) as u32
}

fn ass_geometry_override(geometry: CaptionRenderGeometry, width: u32, height: u32) -> String {
    let center_x_bp = geometry.bounds.x_bp + geometry.bounds.width_bp / 2;
    let center_y_bp = geometry.bounds.y_bp + geometry.bounds.height_bp / 2;
    let x = scale_basis_point(center_x_bp, width);
    let y = scale_basis_point(center_y_bp, height);
    let font_size = scale_basis_point(i32::from(geometry.font_size_bp), height).max(1);
    format!(r"{{\an5\pos({x},{y})\fs{font_size}\q2}}")
}

fn ass_caption_style(preset: AssCaptionPreset, width: u32, height: u32) -> String {
    let font_size = caption_font_size(preset, height);
    let margin_horizontal = ((width as f64 * 0.065).round() as u32).max(36);
    let margin_vertical =
        ((height as f64 * preset.margin_vertical_fraction).round() as u32).max(36);
    format!(
        "Style: {},{},{},{},{},{},{},{},0,0,0,100,100,{},0,{},{},{},{},{},{},{},1\n",
        preset.ass_name,
        preset.font_name,
        font_size,
        preset.primary_color,
        preset.secondary_color,
        preset.outline_color,
        preset.back_color,
        if preset.bold { -1 } else { 0 },
        preset.spacing,
        preset.border_style,
        preset.outline,
        preset.shadow,
        preset.alignment,
        margin_horizontal,
        margin_horizontal,
        margin_vertical,
    )
}

fn append_caption_events(
    document: &mut String,
    manifest: &VideoProjectManifest,
    caption: &CaptionCue,
    width: u32,
    height: u32,
) -> VideoResult<()> {
    let (preset, pages) = planned_caption_pages_for_cue(manifest, caption)?;
    let speaker = caption.speaker_id.as_deref().unwrap_or_default();

    for page in pages {
        let geometry = caption_geometry_for_page(manifest, caption, preset, &page)?;
        let geometry_override = ass_geometry_override(geometry, width, height);
        match preset.reveal {
            CaptionRevealMode::Page => {
                let mut text = caption_page_text(&page, preset, None, None, None);
                let animation = caption_page_animation(preset.id, page.start_us);
                if !animation.is_empty() {
                    text.insert_str(0, animation);
                }
                text.insert_str(0, &geometry_override);
                document.push_str(&ass_dialogue(
                    10,
                    page.start_us,
                    page.end_us,
                    preset.ass_name,
                    speaker,
                    &text,
                ));
            }
            CaptionRevealMode::ActiveWord => {
                append_stateful_caption_events(
                    document,
                    &page,
                    preset,
                    speaker,
                    &geometry_override,
                    true,
                    false,
                )?;
            }
            CaptionRevealMode::Karaoke => {
                append_stateful_caption_events(
                    document,
                    &page,
                    preset,
                    speaker,
                    &geometry_override,
                    false,
                    true,
                )?;
            }
            CaptionRevealMode::Typewriter => {
                append_typewriter_events(document, &page, preset, speaker, &geometry_override)?;
            }
        }
    }
    Ok(())
}

fn planned_caption_pages_for_cue(
    manifest: &VideoProjectManifest,
    caption: &CaptionCue,
) -> VideoResult<(AssCaptionPreset, Vec<CaptionPage>)> {
    let preset = caption_preset(CaptionPresetId::parse(&caption.style_id)?);
    let tokens = timed_caption_tokens(manifest, caption, preset)?;
    let pages = paginate_caption_tokens(tokens, caption.range, preset.paging)?;
    Ok((preset, pages))
}

/// Prefer word ranges mapped through the same canonical source clip used by A/V assembly. When
/// words cannot be matched safely (generated copy, corrections with changed token count, missing
/// scene binding, or conflicting track mappings), pacing falls back deterministically across the
/// existing cue range. The fallback never changes the cue's start or end.
fn timed_caption_tokens(
    manifest: &VideoProjectManifest,
    caption: &CaptionCue,
    preset: AssCaptionPreset,
) -> VideoResult<Vec<TimedCaptionToken>> {
    let raw_tokens = caption
        .text
        .split_whitespace()
        .map(|token| apply_caption_casing(token, preset))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if raw_tokens.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption contains no renderable words",
        )
        .at("captions.text"));
    }

    let exact_ranges = exact_caption_word_ranges(manifest, caption, raw_tokens.len());
    let ranges = exact_ranges.unwrap_or(weighted_ranges(
        caption.range,
        &raw_tokens
            .iter()
            .map(|token| token.chars().count().max(1))
            .collect::<Vec<_>>(),
    )?);
    let mut tokens = raw_tokens
        .into_iter()
        .zip(ranges)
        .enumerate()
        .map(|(index, (text, range))| TimedCaptionToken {
            text,
            space_before: index > 0,
            start_us: range.start_us,
            end_us: range.end_us,
        })
        .collect::<Vec<_>>();
    tokens = split_oversize_caption_tokens(tokens, preset.paging.max_chars_per_line)?;
    Ok(tokens)
}

fn apply_caption_casing(value: &str, preset: AssCaptionPreset) -> String {
    if preset.uppercase {
        value.to_uppercase()
    } else if preset.lowercase {
        value.to_lowercase()
    } else {
        value.to_string()
    }
}

fn exact_caption_word_ranges(
    manifest: &VideoProjectManifest,
    caption: &CaptionCue,
    display_token_count: usize,
) -> Option<Vec<TimeRange>> {
    let transcript = manifest.transcript.as_ref()?;
    let segment_id = caption.transcript_segment_id.as_deref()?;
    let scene_id = caption.scene_id.as_deref()?;
    let segment = transcript
        .segments
        .iter()
        .find(|segment| segment.id == segment_id)?;
    if segment.word_ids.len() != display_token_count || segment.word_ids.is_empty() {
        return None;
    }
    let word_by_id = transcript
        .words
        .iter()
        .map(|word| (word.id.as_str(), word))
        .collect::<BTreeMap<_, _>>();

    let mut ranges = Vec::with_capacity(segment.word_ids.len());
    let mut previous_end = caption.range.start_us;
    for word_id in &segment.word_ids {
        let word = word_by_id.get(word_id.as_str())?;
        let mut mapped_ranges = BTreeSet::new();
        for track in &manifest.tracks {
            if !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
                continue;
            }
            for clip in &track.clips {
                if clip.scene_id.as_deref() != Some(scene_id)
                    || clip.media.source_asset_id.as_deref()
                        != Some(transcript.source_asset_id.as_str())
                    || !clip.source_range.contains_endpoint(word.range.start_us)
                    || !clip.source_range.contains_endpoint(word.range.end_us)
                {
                    continue;
                }
                let start = map_source_endpoint_to_timeline(
                    clip,
                    word.range.start_us,
                    QuantizeMode::Nearest,
                )
                .ok()?;
                let end =
                    map_source_endpoint_to_timeline(clip, word.range.end_us, QuantizeMode::Nearest)
                        .ok()?;
                let start = start.max(caption.range.start_us);
                let end = end.min(caption.range.end_us);
                if end > start {
                    mapped_ranges.insert((start.0, end.0));
                }
            }
        }
        if mapped_ranges.len() != 1 {
            return None;
        }
        let (start, end) = mapped_ranges.into_iter().next()?;
        let range = TimeRange::new(start, end).ok()?;
        // Overlapping or reordered word evidence is not safe for active-word effects. Keep the
        // cue clock intact and use the documented bounded fallback instead.
        if range.start_us < previous_end {
            return None;
        }
        previous_end = range.end_us;
        ranges.push(range);
    }
    Some(ranges)
}

fn weighted_ranges(range: TimeRange, weights: &[usize]) -> VideoResult<Vec<TimeRange>> {
    if weights.is_empty() {
        return Ok(Vec::new());
    }
    let duration = range.duration()?.0;
    if duration < i64::try_from(weights.len()).unwrap_or(i64::MAX) {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption duration is too short for deterministic token pacing",
        )
        .at("captions.range"));
    }
    let total_weight = weights
        .iter()
        .try_fold(0_i128, |total, weight| total.checked_add(*weight as i128))
        .ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "caption token weighting overflowed",
            )
        })?;
    let mut cumulative = 0_i128;
    let mut cursor = range.start_us.0;
    let mut result = Vec::with_capacity(weights.len());
    for (index, weight) in weights.iter().enumerate() {
        cumulative = cumulative.checked_add(*weight as i128).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "caption token weighting overflowed",
            )
        })?;
        let end = if index + 1 == weights.len() {
            range.end_us.0
        } else {
            let offset = (duration as i128).checked_mul(cumulative).ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::ArithmeticOverflow,
                    "caption token timing overflowed",
                )
            })? / total_weight;
            range
                .start_us
                .0
                .checked_add(i64::try_from(offset).map_err(|_| {
                    VideoError::new(
                        VideoErrorCode::ArithmeticOverflow,
                        "caption token timing exceeded the timeline clock",
                    )
                })?)
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::ArithmeticOverflow,
                        "caption token timing overflowed",
                    )
                })?
        };
        if end <= cursor {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCaption,
                "caption duration is too short for positive token timing",
            )
            .at("captions.range"));
        }
        result.push(TimeRange::new(cursor, end)?);
        cursor = end;
    }
    Ok(result)
}

fn split_oversize_caption_tokens(
    tokens: Vec<TimedCaptionToken>,
    max_chars: usize,
) -> VideoResult<Vec<TimedCaptionToken>> {
    let mut result = Vec::new();
    for token in tokens {
        let characters = token.text.chars().collect::<Vec<_>>();
        if characters.len() <= max_chars {
            result.push(token);
            continue;
        }
        let chunks = characters
            .chunks(max_chars)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<_>>();
        let chunk_ranges = weighted_ranges(
            TimeRange::new(token.start_us.0, token.end_us.0)?,
            &chunks
                .iter()
                .map(|chunk| chunk.chars().count())
                .collect::<Vec<_>>(),
        )?;
        for (index, (text, range)) in chunks.into_iter().zip(chunk_ranges).enumerate() {
            result.push(TimedCaptionToken {
                text,
                space_before: index == 0 && token.space_before,
                start_us: range.start_us,
                end_us: range.end_us,
            });
        }
    }
    Ok(result)
}

fn paginate_caption_tokens(
    tokens: Vec<TimedCaptionToken>,
    cue_range: TimeRange,
    rules: CaptionPagingRules,
) -> VideoResult<Vec<CaptionPage>> {
    let mut grouped = Vec::<Vec<TimedCaptionToken>>::new();
    let mut current = Vec::<TimedCaptionToken>::new();
    for token in tokens {
        if !current.is_empty() && !caption_page_accepts(&current, &token, rules) {
            grouped.push(std::mem::take(&mut current));
        }
        current.push(token);
        if rules.break_on_punctuation
            && current.len() >= 2
            && current
                .last()
                .is_some_and(|token| caption_token_ends_phrase(&token.text))
        {
            grouped.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        grouped.push(current);
    }
    if grouped.len() >= 2 && grouped.last().is_some_and(|page| page.len() == 1) {
        let last = grouped.pop().expect("checked trailing caption page");
        let candidate = last.first().expect("one-token trailing caption page");
        let can_merge = grouped
            .last()
            .is_some_and(|prior| caption_page_accepts(prior, candidate, rules));
        if can_merge {
            grouped.last_mut().expect("prior caption page").extend(last);
        } else {
            grouped.push(last);
        }
    }
    if grouped.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption paging produced no display pages",
        ));
    }

    let duration = cue_range.duration()?.0;
    let page_count = i64::try_from(grouped.len()).map_err(|_| {
        VideoError::new(
            VideoErrorCode::ArithmeticOverflow,
            "caption page count exceeded the timeline clock",
        )
    })?;
    let minimum_total = page_count
        .checked_mul(MIN_ASS_PAGE_DURATION_US)
        .ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "caption page duration overflowed",
            )
        })?;
    if duration < minimum_total {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption text cannot be safely paged inside its cue duration",
        )
        .at("captions.range"));
    }
    let minimum_page_duration = rules
        .preferred_min_page_us
        .min(duration / page_count)
        .max(MIN_ASS_PAGE_DURATION_US);
    let mut boundaries = vec![cue_range.start_us];
    for index in 1..grouped.len() {
        let previous = *boundaries.last().expect("caption start boundary");
        let remaining = i64::try_from(grouped.len() - index).map_err(|_| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "caption page count exceeded the timeline clock",
            )
        })?;
        let lower = previous.checked_add(Microseconds(minimum_page_duration))?;
        let reserved = remaining
            .checked_mul(minimum_page_duration)
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::ArithmeticOverflow,
                    "caption page duration overflowed",
                )
            })?;
        let upper = Microseconds(cue_range.end_us.0.checked_sub(reserved).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "caption page boundary overflowed",
            )
        })?);
        let desired = grouped[index]
            .first()
            .expect("non-empty caption page")
            .start_us;
        boundaries.push(desired.max(lower).min(upper));
    }
    boundaries.push(cue_range.end_us);

    grouped
        .into_iter()
        .enumerate()
        .map(|(index, mut tokens)| {
            let start_us = boundaries[index];
            let end_us = boundaries[index + 1];
            if end_us <= start_us {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidCaption,
                    "caption pages must have positive, non-overlapping durations",
                )
                .at("captions.range"));
            }
            let safely_inside_page = tokens.iter().all(|token| {
                token.start_us >= start_us
                    && token.end_us <= end_us
                    && token.end_us > token.start_us
            }) && tokens
                .windows(2)
                .all(|pair| pair[0].end_us <= pair[1].start_us);
            if !safely_inside_page {
                // Minimum readable page spacing can conflict with unusually clustered source-word
                // evidence. In that case the exact timing is no longer safe for this page; pace
                // only its own words inside the already-authoritative page bounds.
                let ranges = weighted_ranges(
                    TimeRange::new(start_us.0, end_us.0)?,
                    &tokens
                        .iter()
                        .map(|token| token.text.chars().count().max(1))
                        .collect::<Vec<_>>(),
                )?;
                for (token, range) in tokens.iter_mut().zip(ranges) {
                    token.start_us = range.start_us;
                    token.end_us = range.end_us;
                }
            }
            Ok(CaptionPage {
                tokens,
                start_us,
                end_us,
            })
        })
        .collect()
}

fn caption_page_accepts(
    current: &[TimedCaptionToken],
    candidate: &TimedCaptionToken,
    rules: CaptionPagingRules,
) -> bool {
    if current.len() >= rules.max_words {
        return false;
    }
    let mut proposed = current.to_vec();
    proposed.push(candidate.clone());
    caption_line_breaks(&proposed, rules).is_some()
}

fn caption_line_breaks(
    tokens: &[TimedCaptionToken],
    rules: CaptionPagingRules,
) -> Option<Vec<bool>> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut line = 1_usize;
    let mut line_chars = 0_usize;
    for (index, token) in tokens.iter().enumerate() {
        let token_chars = token.text.chars().count();
        if token_chars == 0 || token_chars > rules.max_chars_per_line {
            return None;
        }
        let separator = usize::from(index > 0 && token.space_before);
        if line_chars > 0 && line_chars + separator + token_chars > rules.max_chars_per_line {
            line += 1;
            line_chars = token_chars;
            result.push(true);
        } else {
            line_chars += separator + token_chars;
            result.push(false);
        }
        if line > rules.max_lines {
            return None;
        }
    }
    Some(result)
}

fn caption_token_ends_phrase(value: &str) -> bool {
    value
        .chars()
        .last()
        .is_some_and(|character| matches!(character, '.' | '!' | '?' | '…' | ',' | ';' | ':' | '—'))
}

fn append_stateful_caption_events(
    document: &mut String,
    page: &CaptionPage,
    preset: AssCaptionPreset,
    speaker: &str,
    geometry_override: &str,
    active_word: bool,
    progressive: bool,
) -> VideoResult<()> {
    let mut boundaries = BTreeSet::from([page.start_us, page.end_us]);
    for token in &page.tokens {
        boundaries.insert(token.start_us.max(page.start_us).min(page.end_us));
        if active_word {
            boundaries.insert(token.end_us.max(page.start_us).min(page.end_us));
        }
    }
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if end <= start || ass_time(start) == ass_time(end) {
            continue;
        }
        let midpoint = Microseconds(start.0 + (end.0 - start.0) / 2);
        let active = active_word.then(|| {
            page.tokens
                .iter()
                .position(|token| token.start_us <= midpoint && midpoint < token.end_us)
        });
        let active = active.flatten();
        let spoken = progressive.then(|| {
            page.tokens
                .iter()
                .take_while(|token| token.start_us <= midpoint)
                .count()
        });
        let text = format!(
            "{geometry_override}{}",
            caption_page_text(page, preset, active, spoken, None)
        );
        document.push_str(&ass_dialogue(
            10,
            start,
            end,
            preset.ass_name,
            speaker,
            &text,
        ));
    }
    Ok(())
}

fn append_typewriter_events(
    document: &mut String,
    page: &CaptionPage,
    preset: AssCaptionPreset,
    speaker: &str,
    geometry_override: &str,
) -> VideoResult<()> {
    let mut boundaries = BTreeSet::from([page.start_us, page.end_us]);
    boundaries.extend(
        page.tokens
            .iter()
            .map(|token| token.start_us.max(page.start_us).min(page.end_us)),
    );
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if end <= start || ass_time(start) == ass_time(end) {
            continue;
        }
        let midpoint = Microseconds(start.0 + (end.0 - start.0) / 2);
        let visible = page
            .tokens
            .iter()
            .take_while(|token| token.start_us <= midpoint)
            .count();
        if visible == 0 {
            continue;
        }
        let text = format!(
            "{geometry_override}{}",
            caption_page_text(page, preset, None, None, Some(visible))
        );
        document.push_str(&ass_dialogue(
            10,
            start,
            end,
            preset.ass_name,
            speaker,
            &text,
        ));
    }
    Ok(())
}

fn caption_page_text(
    page: &CaptionPage,
    preset: AssCaptionPreset,
    active_token: Option<usize>,
    spoken_token_count: Option<usize>,
    visible_token_count: Option<usize>,
) -> String {
    let visible = visible_token_count
        .unwrap_or(page.tokens.len())
        .min(page.tokens.len());
    let breaks = caption_line_breaks(&page.tokens, preset.paging)
        .expect("caption pages are measured before rendering");
    let mut result = String::new();
    let mut current_color: Option<&str> = None;
    for (index, token) in page.tokens.iter().take(visible).enumerate() {
        if index > 0 {
            if breaks[index] {
                result.push_str(r"\N");
            } else if token.space_before {
                result.push(' ');
            }
        }
        let color = if active_token == Some(index)
            || spoken_token_count.is_some_and(|count| index < count)
        {
            preset.active_color
        } else {
            preset.primary_color
        };
        if current_color != Some(color) {
            result.push_str(&format!("{{\\1c{color}}}"));
            current_color = Some(color);
        }
        result.push_str(&escape_ass_text(&token.text));
    }
    result
}

fn plain_caption_page_text(page: &CaptionPage, preset: AssCaptionPreset) -> String {
    let breaks = caption_line_breaks(&page.tokens, preset.paging)
        .expect("caption pages are measured before projection");
    let mut result = String::new();
    for (index, token) in page.tokens.iter().enumerate() {
        if index > 0 {
            if breaks[index] {
                result.push('\n');
            } else if token.space_before {
                result.push(' ');
            }
        }
        result.push_str(&token.text);
    }
    result
}

fn caption_page_animation(id: CaptionPresetId, start: Microseconds) -> &'static str {
    // Like Reelmify, a page visible on the first output frame is stable: it does not fade/pop in.
    if start == Microseconds::ZERO {
        return "";
    }
    match id {
        CaptionPresetId::Calm => r"{\fad(120,160)}",
        CaptionPresetId::Kinetic => r"{\fad(40,80)\fscx104\fscy104\t(0,140,\fscx100\fscy100)}",
        CaptionPresetId::Podcast => r"{\fad(100,160)}",
        _ => "",
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

/// The canvas an episode renders on. Exposed so generated artwork is drawn at exactly the
/// canvas it will be composited onto rather than at a size guessed alongside it.
pub fn profile_dimensions(profile: RenderProfile, layout: &LayoutPlan) -> (u32, u32) {
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
    #[test]
    #[ignore = "prints the catalog for regenerating the browser-preview fixture"]
    fn dump_caption_preset_catalog() {
        println!(
            "{}",
            serde_json::to_string_pretty(&present_caption_presets()).unwrap()
        );
    }

    #[test]
    fn ass_colours_become_css_with_inverted_alpha() {
        // ASS stores &HAABBGGRR: channels reversed, and alpha is transparency, so 00 is opaque.
        assert_eq!(
            ass_color_to_css("&H00FFFFFF"),
            json!("rgba(255, 255, 255, 1.000)")
        );
        assert_eq!(
            ass_color_to_css("&H003DD9FF"),
            json!("rgba(255, 217, 61, 1.000)")
        );
        assert_eq!(
            ass_color_to_css("&H50000000"),
            json!("rgba(0, 0, 0, 0.686)")
        );
        assert_eq!(
            ass_color_to_css("&HFF000000"),
            json!("rgba(0, 0, 0, 0.000)")
        );
        assert_eq!(ass_color_to_css("not-a-colour"), Value::Null);
    }

    #[test]
    fn every_preset_is_presented_with_a_renderable_web_spec() {
        let presets = present_caption_presets();
        assert_eq!(presets.len(), CaptionPresetId::ALL.len());
        for (preset, id) in presets.iter().zip(CaptionPresetId::ALL) {
            assert_eq!(preset["id"], json!(id.public_id()));
            assert!(preset["label"].as_str().is_some_and(|l| !l.is_empty()));
            assert!(preset["text_color"]
                .as_str()
                .is_some_and(|c| c.starts_with("rgba(")));
            assert!(preset["active_color"]
                .as_str()
                .is_some_and(|c| c.starts_with("rgba(")));
            let size = preset["relative_size"].as_f64().expect("relative size");
            assert!((0.02..0.10).contains(&size), "{id:?} size {size}");
            assert!(["page", "active-word", "karaoke", "typewriter"]
                .contains(&preset["reveal"].as_str().expect("reveal")));
            assert!(
                ["as-is", "upper", "lower"].contains(&preset["casing"].as_str().expect("casing"))
            );
        }
    }

    #[test]
    fn only_opaque_box_presets_carry_a_background() {
        let presets = present_caption_presets();
        let background = |id: &str| {
            presets
                .iter()
                .find(|preset| preset["id"] == json!(id))
                .expect("preset")["background_color"]
                .clone()
        };
        // libass paints back_color only in opaque-box mode (border_style 3). Presets that draw an
        // outline instead must not grow a pill in the preview that the export never renders.
        assert_ne!(background("typewriter"), Value::Null);
        assert_ne!(background("podcast"), Value::Null);
        for outlined in [
            "clean-white",
            "calm",
            "kinetic",
            "bold-pop",
            "highlight",
            "karaoke",
        ] {
            assert_eq!(
                background(outlined),
                Value::Null,
                "{outlined} must not paint a box"
            );
        }
    }

    #[test]
    fn presets_that_emphasise_words_expose_a_distinct_active_colour() {
        let presets = present_caption_presets();
        for preset in presets.iter().filter(|p| p["reveal"] != json!("page")) {
            if preset["reveal"] == json!("typewriter") {
                continue;
            }
            assert_ne!(
                preset["active_color"], preset["text_color"],
                "{} reveals words but cannot show which one is active",
                preset["id"]
            );
        }
    }

    use super::super::contracts::{
        default_caption_bounds, AudioMix, AudioMixTrack, CanvasSpec, GapReason, LayoutElement,
        LayoutPlan, MediaProbe, MediaReference, NormalizedRect, Provenance, ProvenanceKind,
        RationalFrameRate, RationalRate, ReviewState, ReviewedScene, SourceAsset, SourceAssetKind,
        TimeRange, TimelineClip, TimelineGap, TimelineTrack, TrackKind, TranscriptSegment,
        TranscriptTimingSource, TranscriptVersion, TranscriptWord,
    };
    use super::super::visuals::{
        VisualAsset, VisualEasing, VisualFit, VisualLayer, VisualMimeType, VisualMotion,
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
            turn_id: None,
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
            turn_id: None,
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
    fn every_curated_caption_preset_has_one_distinct_ass_style_and_round_trips() {
        let root =
            std::env::temp_dir().join(format!("soundar-presets-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        let mut style_lines = BTreeSet::new();

        for id in CaptionPresetId::ALL {
            manifest.captions[0].style_id = id.manifest_id().into();
            let document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();
            let preset = caption_preset(id);
            assert_eq!(
                document
                    .lines()
                    .filter(|line| line.starts_with(&format!("Style: {},", preset.ass_name)))
                    .count(),
                1,
                "{} must be emitted exactly once",
                id.public_id()
            );
            assert!(document.lines().any(|line| {
                line.starts_with("Dialogue: 10,") && line.split(',').nth(3) == Some(preset.ass_name)
            }));
            style_lines.insert(
                document
                    .lines()
                    .find(|line| line.starts_with(&format!("Style: {},", preset.ass_name)))
                    .unwrap()
                    .to_string(),
            );
        }
        assert_eq!(style_lines.len(), CaptionPresetId::ALL.len());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mixed_per_cue_styles_render_without_a_global_theme_override() {
        let root =
            std::env::temp_dir().join(format!("soundar-mixed-ass-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.captions[0].range = TimeRange::new(100_000, 480_000).unwrap();
        manifest.captions[0].text = "FIRST IDEA".into();
        manifest.captions[0].style_id = "caption-bold-pop".into();
        let mut second = manifest.captions[0].clone();
        second.id = "caption-2".into();
        second.range = TimeRange::new(480_000, 900_000).unwrap();
        second.text = "a measured response".into();
        second.style_id = "caption-podcast".into();
        manifest.captions.push(second);
        manifest.validate_strict().unwrap();

        let document = build_ass_document(
            &manifest,
            &AssemblyOptions {
                caption_theme: CaptionTheme::Kinetic,
                ..AssemblyOptions::default()
            },
        )
        .unwrap();
        assert!(document.contains(",CaptionBoldPop,Host,"));
        assert!(document.contains(",CaptionPodcast,Host,"));
        assert!(!document.contains("Style: Caption,"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn long_caption_text_pages_within_bounds_and_never_exceeds_line_limits() {
        let root = std::env::temp_dir().join(format!("soundar-paging-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.captions[0].range = TimeRange::new(20_000, 980_000).unwrap();
        manifest.captions[0].style_id = "caption-podcast".into();
        manifest.captions[0].text = "One thoughtful question can open a conversation, reveal an overlooked detail, connect an earlier idea, and leave the audience with a clear next step. extraordinarilylongunbrokenidentifierthatmustneverescape".into();
        let caption = manifest.captions[0].clone();
        let preset = caption_preset(CaptionPresetId::Podcast);
        let tokens = timed_caption_tokens(&manifest, &caption, preset).unwrap();
        let pages = paginate_caption_tokens(tokens, caption.range, preset.paging).unwrap();
        assert!(pages.len() >= 3);
        assert_eq!(pages.first().unwrap().start_us, caption.range.start_us);
        assert_eq!(pages.last().unwrap().end_us, caption.range.end_us);
        for pair in pages.windows(2) {
            assert_eq!(pair[0].end_us, pair[1].start_us);
            assert!(pair[0].end_us > pair[0].start_us);
        }
        for page in &pages {
            let breaks = caption_line_breaks(&page.tokens, preset.paging).unwrap();
            assert!(breaks.iter().filter(|value| **value).count() < 2);
            let mut line_chars = 0;
            for (index, token) in page.tokens.iter().enumerate() {
                if breaks[index] {
                    assert!(line_chars <= preset.paging.max_chars_per_line);
                    line_chars = token.text.chars().count();
                } else {
                    line_chars +=
                        usize::from(index > 0 && token.space_before) + token.text.chars().count();
                }
            }
            assert!(line_chars <= preset.paging.max_chars_per_line);
        }
        let document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();
        assert!(document.matches(",CaptionPodcast,Host,").count() >= pages.len());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn exact_source_words_drive_active_timing_and_mismatch_uses_bounded_fallback() {
        let root =
            std::env::temp_dir().join(format!("soundar-word-clock-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.transcript = Some(TranscriptVersion {
            id: "transcript-1".into(),
            source_asset_id: "source-1".into(),
            source_clock_duration_us: Microseconds(1_000_000),
            language: Some("en".into()),
            timing_source: TranscriptTimingSource::Manual,
            preserved_source_gaps: true,
            segments: vec![TranscriptSegment {
                id: "segment-1".into(),
                range: TimeRange::new(100_000, 900_000).unwrap(),
                text: "Hello world".into(),
                speaker_id: Some("Host".into()),
                word_ids: vec!["word-1".into(), "word-2".into()],
            }],
            words: vec![
                TranscriptWord {
                    id: "word-1".into(),
                    range: TimeRange::new(200_000, 320_000).unwrap(),
                    text: "Hello".into(),
                    speaker_id: Some("Host".into()),
                    confidence_milli: Some(990),
                },
                TranscriptWord {
                    id: "word-2".into(),
                    range: TimeRange::new(500_000, 650_000).unwrap(),
                    text: "world".into(),
                    speaker_id: Some("Host".into()),
                    confidence_milli: Some(980),
                },
            ],
            content_sha256: "e".repeat(64),
            created_at: timestamp(),
        });
        manifest.captions[0].range = TimeRange::new(100_000, 900_000).unwrap();
        manifest.captions[0].text = "Hello world".into();
        manifest.captions[0].style_id = "caption-highlight".into();
        manifest.captions[0].transcript_segment_id = Some("segment-1".into());
        manifest.validate_strict().unwrap();

        let preset = caption_preset(CaptionPresetId::Highlight);
        let exact = timed_caption_tokens(&manifest, &manifest.captions[0], preset).unwrap();
        assert_eq!(exact[0].start_us, Microseconds(200_000));
        assert_eq!(exact[0].end_us, Microseconds(320_000));
        assert_eq!(exact[1].start_us, Microseconds(500_000));
        assert_eq!(exact[1].end_us, Microseconds(650_000));
        let document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();
        assert!(document.contains("0:00:00.20,0:00:00.32,CaptionHighlight"));
        assert!(document.contains("0:00:00.50,0:00:00.65,CaptionHighlight"));

        manifest.captions[0].text = "Hello brave world".into();
        let fallback = timed_caption_tokens(&manifest, &manifest.captions[0], preset).unwrap();
        assert_eq!(fallback.first().unwrap().start_us, Microseconds(100_000));
        assert_eq!(fallback.last().unwrap().end_us, Microseconds(900_000));
        assert!(fallback
            .windows(2)
            .all(|pair| pair[0].end_us == pair[1].start_us));
        assert!(fallback.iter().all(|token| {
            token.start_us >= manifest.captions[0].range.start_us
                && token.end_us <= manifest.captions[0].range.end_us
                && token.end_us > token.start_us
        }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn caption_geometry_is_authoritative_without_changing_page_or_word_clocks() {
        let root = std::env::temp_dir().join(format!(
            "soundar-caption-geometry-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        fs::write(&source, b"fixture").unwrap();
        let mut manifest = fixture_manifest(&source);
        manifest.captions[0].style_id = "caption-karaoke".into();

        let default_pages = plan_caption_preview_pages(&manifest).unwrap();
        let expected_default = default_caption_bounds(&manifest.layout).unwrap();
        assert!(default_pages
            .iter()
            .all(|page| page.bounds == expected_default));
        let default_document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();

        let moved = NormalizedRect {
            x_bp: 100,
            y_bp: 100,
            width_bp: 3_000,
            height_bp: 1_200,
        };
        manifest.layout.elements.push(LayoutElement {
            id: "caption-layout-scene-1".into(),
            role: LayoutRole::Captions,
            scene_id: Some("scene-1".into()),
            bounds: moved,
            z_index: 100,
            style_id: None,
        });
        manifest.validate_strict().unwrap();
        let moved_pages = plan_caption_preview_pages(&manifest).unwrap();
        assert!(moved_pages.iter().all(|page| page.bounds == moved));
        assert_ne!(
            moved_pages[0].font_size_bp, default_pages[0].font_size_bp,
            "resize must bind the authoritative font size"
        );
        let page_clock = |pages: &[CaptionPreviewPage]| {
            pages
                .iter()
                .map(|page| {
                    (
                        page.id.clone(),
                        page.start_us,
                        page.end_us,
                        page.text.clone(),
                        page.words
                            .iter()
                            .map(|word| (word.text.clone(), word.start_us, word.end_us))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(page_clock(&default_pages), page_clock(&moved_pages));

        let moved_document = build_ass_document(&manifest, &AssemblyOptions::default()).unwrap();
        // Preview is 720x1280: center (1600,700) basis points maps to (115,90).
        assert!(moved_document.contains(r"{\an5\pos(115,90)\fs"));
        let event_clock = |document: &str| {
            document
                .lines()
                .filter(|line| line.starts_with("Dialogue: 10,"))
                .map(|line| line.splitn(6, ',').take(5).collect::<Vec<_>>().join(","))
                .collect::<Vec<_>>()
        };
        assert_eq!(event_clock(&default_document), event_clock(&moved_document));
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
    fn real_ffmpeg_animates_a_still_visual_layer_on_the_canonical_clock() {
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let ffprobe = Path::new("/usr/bin/ffprobe");
        if !ffmpeg.is_file() || !ffprobe.is_file() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("soundar-visual-layer-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let illustration = root.join("illustration.png");
        let output = root.join("output.mp4");
        let source_status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x142033:s=320x180:r=30:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:sample_rate=48000:duration=1",
                "-shortest",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(source_status.success());
        let image_status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0xE85D45@1:s=128x128:d=0.04",
                "-frames:v",
                "1",
                "-threads",
                "1",
            ])
            .arg(&illustration)
            .status()
            .unwrap();
        assert!(image_status.success());

        let mut manifest = fixture_manifest(&source);
        let illustration_size = fs::metadata(&illustration).unwrap().len();
        manifest.visual_assets.push(VisualAsset {
            id: "visual-illustration".into(),
            managed_path: "illustration.png".into(),
            sha256: "b".repeat(64),
            mime_type: VisualMimeType::Png,
            width: 128,
            height: 128,
            has_alpha: false,
            size_bytes: illustration_size,
            provenance: Provenance {
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: timestamp(),
                producer: "assembly-test-image-generator".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::new(),
            },
            created_at: timestamp(),
        });
        manifest.visual_layers.push(VisualLayer {
            id: "layer-illustration".into(),
            asset_id: "visual-illustration".into(),
            scene_id: Some("scene-1".into()),
            range: TimeRange::new(0, 1_000_000).unwrap(),
            fit: VisualFit::Cover,
            crop: None,
            z_index: 10,
            motion: VisualMotion {
                start_bounds: NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 2_000,
                    height_bp: 2_000,
                },
                end_bounds: NormalizedRect {
                    x_bp: 6_000,
                    y_bp: 7_000,
                    width_bp: 3_000,
                    height_bp: 3_000,
                },
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: VisualEasing::EaseInOut,
            },
            transition_in_us: Microseconds(100_000),
            transition_out_us: Microseconds(100_000),
        });
        manifest.validate_strict().unwrap();
        let sources = BTreeMap::from([
            ("source:source-1".into(), source),
            ("visual:visual-illustration".into(), illustration),
        ]);
        let plan = build_timeline_render_plan(
            ffmpeg,
            &manifest,
            &sources,
            None,
            &output,
            &AssemblyOptions {
                profile: RenderProfile::Preview,
                burn_captions: false,
                ..AssemblyOptions::default()
            },
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
        assert!(filter.contains("visual_source_0"));
        assert!(filter.contains("scale=w='max(2,trunc"));
        assert!(filter.contains("overlay=x='"));
        assert!(filter.contains("fade=t=in"));
        assert!(filter.contains("fade=t=out"));
        let result = plan
            .primary
            .command()
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
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
        assert!(value["streams"].as_array().unwrap().iter().any(|stream| {
            stream["codec_type"] == "video" && stream["width"] == 720 && stream["height"] == 1280
        }));
        let frame_digest = |at: &str| {
            let result = Command::new(ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-ss", at, "-i"])
                .arg(&output)
                .args(["-frames:v", "1", "-f", "framemd5", "-"])
                .output()
                .unwrap();
            assert!(result.status.success());
            result.stdout
        };
        assert_ne!(frame_digest("0.15"), frame_digest("0.85"));
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
        let mut manifest = fixture_manifest(&source);
        manifest.captions[0].style_id = "caption-karaoke".into();
        manifest.layout.elements.push(LayoutElement {
            id: "caption-layout-smoke".into(),
            role: LayoutRole::Captions,
            scene_id: Some("scene-1".into()),
            bounds: NormalizedRect {
                x_bp: 1_000,
                y_bp: 1_200,
                width_bp: 4_000,
                height_bp: 1_000,
            },
            z_index: 100,
            style_id: None,
        });
        let options = AssemblyOptions {
            profile: RenderProfile::Proxy,
            portrait_layout: PortraitLayout::Contain,
            caption_theme: CaptionTheme::Calm,
            ..AssemblyOptions::default()
        };
        let captions_path = root.join("captions.ass");
        let ass = build_ass_document(&manifest, &options).unwrap();
        // Proxy is 540x960: center (3000,1700) basis points maps to (162,163).
        assert!(ass.contains(r"{\an5\pos(162,163)\fs"));
        write_ass_document_atomic(&captions_path, &ass).unwrap();
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
