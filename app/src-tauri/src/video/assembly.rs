//! FFmpeg assembly planning for reviewed Video Studio timelines.
//!
//! Scene timing is always read from the canonical manifest. Each selected scene is sought from
//! its original source clock, deliberate timeline holes become explicit black/silent segments,
//! and captions/title/speaker cards are burned from a generated ASS document. Commands never
//! involve a shell, and both NVENC and software plans are deterministic.

use super::contracts::{
    CanvasMode, CaptionCue, LayoutRole, Microseconds, ReviewedScene, VideoError, VideoErrorCode,
    VideoProjectManifest, VideoResult,
};
use super::renderer::{
    PortraitLayout, RenderCommand, RenderCommandPlan, RenderProfile, RenderWorkloadClass,
    VideoEncoder,
};
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
/// use the same source/range data to pre-render unchanged segments, then pass those artifacts back
/// as source assets without changing timeline math.
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

    let mut scenes = manifest.reviewed_scenes.iter().collect::<Vec<_>>();
    scenes.sort_by_key(|scene| (scene.timeline_start_us, scene.id.as_str()));
    if scenes.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidScene,
            "rendering requires at least one reviewed scene",
        ));
    }
    let mut prior_end = Microseconds::ZERO;
    for scene in &scenes {
        if scene.timeline_start_us < prior_end {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "reviewed scenes overlap on the render timeline",
            ));
        }
        prior_end = scene
            .timeline_start_us
            .checked_add(scene.timeline_duration_us)?;
    }

    let (width, height) = profile_dimensions(options.profile, &manifest.layout.mode);
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

    let mut input_sources = Vec::with_capacity(scenes.len());
    for scene in &scenes {
        let source_id = scene.source_asset_id.as_deref().ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::InvalidScene,
                format!("scene {} has no renderable source", scene.id),
            )
        })?;
        let source = source_by_id.get(source_id).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::MissingReference,
                format!("scene {} references a missing source", scene.id),
            )
        })?;
        let range = scene.source_range.ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::InvalidScene,
                format!("scene {} has no source-clock range", scene.id),
            )
        })?;
        let path = resolved_sources.get(source_id).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::MissingReference,
                format!("source {source_id} has no resolved managed file"),
            )
        })?;
        let path = fs::canonicalize(path).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidAsset,
                format!("source {source_id} could not be opened: {error}"),
            )
        })?;
        if !path.is_file() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                format!("source {source_id} is not a regular file"),
            ));
        }
        args.extend([
            OsString::from("-ss"),
            OsString::from(ffmpeg_time(range.start_us)),
            OsString::from("-t"),
            OsString::from(ffmpeg_time(scene.timeline_duration_us)),
            OsString::from("-i"),
            path.as_os_str().to_os_string(),
        ]);
        input_sources.push(*source);
    }
    args.extend([
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-map_chapters"),
        OsString::from("-1"),
    ]);

    let waveform_requested = manifest
        .layout
        .elements
        .iter()
        .any(|element| matches!(element.role, LayoutRole::Waveform));
    let mut filters = Vec::new();
    let mut segments = Vec::new();
    let mut cursor = Microseconds::ZERO;
    for (input_index, (scene, source)) in scenes.iter().zip(input_sources.iter()).enumerate() {
        if scene.timeline_start_us > cursor {
            let duration = Microseconds(scene.timeline_start_us.0 - cursor.0);
            let gap_index = segments.len();
            append_gap_filters(
                &mut filters,
                &mut segments,
                gap_index,
                duration,
                width,
                height,
                &frame_rate,
            );
        }
        let segment_index = segments.len();
        append_scene_filters(
            &mut filters,
            &mut segments,
            segment_index,
            input_index,
            scene,
            source.probe.has_video,
            source.probe.has_audio,
            waveform_requested,
            width,
            height,
            &frame_rate,
            options.portrait_layout,
        )?;
        cursor = scene
            .timeline_start_us
            .checked_add(scene.timeline_duration_us)?;
    }
    if cursor < manifest.timeline_duration_us {
        let gap_index = segments.len();
        append_gap_filters(
            &mut filters,
            &mut segments,
            gap_index,
            Microseconds(manifest.timeline_duration_us.0 - cursor.0),
            width,
            height,
            &frame_rate,
        );
    }
    if cursor > manifest.timeline_duration_us {
        return Err(VideoError::new(
            VideoErrorCode::DurationMismatch,
            "reviewed scenes exceed the manifest timeline duration",
        ));
    }

    let concat_inputs = segments
        .iter()
        .map(|(video, audio)| format!("[{video}][{audio}]"))
        .collect::<String>();
    filters.push(format!(
        "{concat_inputs}concat=n={}:v=1:a=1[assembled_video][assembled_audio]",
        segments.len()
    ));
    let video_output = if options.burn_captions {
        if let Some(path) = subtitles.as_deref() {
            filters.push(format!(
                "[assembled_video]subtitles=filename='{}'[video_output]",
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
        "[assembled_audio]loudnorm=I={target_lufs:.1}:TP={true_peak:.1}:LRA=11[audio_output]"
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
    let (width, height) = profile_dimensions(options.profile, &manifest.layout.mode);
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

#[allow(clippy::too_many_arguments)]
fn append_scene_filters(
    filters: &mut Vec<String>,
    segments: &mut Vec<(String, String)>,
    segment_index: usize,
    input_index: usize,
    scene: &ReviewedScene,
    has_video: bool,
    has_audio: bool,
    waveform_requested: bool,
    width: u32,
    height: u32,
    frame_rate: &str,
    layout: PortraitLayout,
) -> VideoResult<()> {
    if !has_video && !has_audio {
        return Err(VideoError::new(
            VideoErrorCode::InvalidAsset,
            "a scene source must contain video or audio",
        ));
    }
    let duration = ffmpeg_time(scene.timeline_duration_us);
    let video_label = format!("vseg{segment_index}");
    let audio_label = format!("aseg{segment_index}");
    if has_video {
        let video_filter = match layout {
            PortraitLayout::CenterCrop => format!(
                "[{input_index}:v:0]setpts=PTS-STARTPTS,scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},setsar=1,fps={frame_rate}[{video_label}]"
            ),
            PortraitLayout::Contain => format!(
                "[{input_index}:v:0]setpts=PTS-STARTPTS,scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=0x181818,setsar=1,fps={frame_rate}[{video_label}]"
            ),
            PortraitLayout::BlurPad => format!(
                "[{input_index}:v:0]setpts=PTS-STARTPTS,split=2[vbg{segment_index}][vfg{segment_index}];[vbg{segment_index}]scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},boxblur=luma_radius=24:luma_power=1[bg{segment_index}];[vfg{segment_index}]scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2[fg{segment_index}];[bg{segment_index}][fg{segment_index}]overlay=(W-w)/2:(H-h)/2,setsar=1,fps={frame_rate}[{video_label}]"
            ),
        };
        filters.push(video_filter);
    } else {
        filters.push(format!(
            "color=c=0x181818:s={width}x{height}:r={frame_rate}:d={duration}[bg{segment_index}]"
        ));
        filters.push(format!(
            "[{input_index}:a:0]asplit=2[wave_source{segment_index}][audio_source{segment_index}];[wave_source{segment_index}]asetpts=PTS-STARTPTS,showwaves=s={}x{}:mode=line:colors=0xE5E7EB@0.75:rate={frame_rate},format=rgba[wave{segment_index}]",
            width.saturating_sub(width / 6),
            (height / 4).max(96),
        ));
        filters.push(format!(
            "[bg{segment_index}][wave{segment_index}]overlay=(W-w)/2:(H-h)/2,format=yuv420p[{video_label}]"
        ));
    }
    if has_audio {
        let audio_source = if has_video {
            format!("[{input_index}:a:0]")
        } else {
            format!("[audio_source{segment_index}]")
        };
        filters.push(format!(
            "{audio_source}asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[{audio_label}]"
        ));
        if has_video && waveform_requested {
            // Waveform overlays are represented in the manifest and generated as cached assets;
            // the final live overlay is intentionally reserved for audio-only podcast scenes to
            // avoid consuming the same audio pad twice without a costly split graph.
        }
    } else {
        filters.push(format!(
            "anullsrc=r=48000:cl=stereo,atrim=duration={duration},asetpts=PTS-STARTPTS[{audio_label}]"
        ));
    }
    segments.push((video_label, audio_label));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_gap_filters(
    filters: &mut Vec<String>,
    segments: &mut Vec<(String, String)>,
    segment_index: usize,
    duration: Microseconds,
    width: u32,
    height: u32,
    frame_rate: &str,
) {
    let video_label = format!("vseg{segment_index}");
    let audio_label = format!("aseg{segment_index}");
    let duration = ffmpeg_time(duration);
    filters.push(format!(
        "color=c=0x181818:s={width}x{height}:r={frame_rate}:d={duration}[{video_label}]"
    ));
    filters.push(format!(
        "anullsrc=r=48000:cl=stereo,atrim=duration={duration},asetpts=PTS-STARTPTS[{audio_label}]"
    ));
    segments.push((video_label, audio_label));
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
        let end = end.min(caption.range.end_us.max(end));
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

fn profile_dimensions(profile: RenderProfile, mode: &CanvasMode) -> (u32, u32) {
    match (profile, mode) {
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
        AudioMix, AudioMixTrack, CanvasSpec, LayoutPlan, MediaProbe, MediaReference,
        NormalizedRect, Provenance, ProvenanceKind, RationalFrameRate, RationalRate, ReviewState,
        SourceAsset, SourceAssetKind, TimeRange, TimelineClip, TimelineTrack, TrackKind,
    };
    use super::*;
    use std::process::{Command, Stdio};

    fn timestamp() -> String {
        "2026-08-27T20:00:00.000Z".into()
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
}
