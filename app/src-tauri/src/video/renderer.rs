use super::media::{is_executable_file, local_media_input_args, MediaError};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderProfile {
    Proxy,
    Preview,
    Final,
}

impl RenderProfile {
    fn landscape_dimensions(self) -> (u32, u32) {
        match self {
            Self::Proxy => (960, 540),
            Self::Preview => (1280, 720),
            Self::Final => (3840, 2160),
        }
    }

    fn portrait_dimensions(self) -> (u32, u32) {
        match self {
            Self::Proxy => (540, 960),
            Self::Preview => (720, 1280),
            Self::Final => (1080, 1920),
        }
    }

    fn frame_rate(self) -> u32 {
        match self {
            Self::Proxy | Self::Preview => 30,
            Self::Final => 30,
        }
    }

    fn software_preset(self) -> &'static str {
        match self {
            Self::Proxy => "ultrafast",
            Self::Preview => "veryfast",
            Self::Final => "medium",
        }
    }

    fn software_crf(self) -> &'static str {
        match self {
            Self::Proxy => "28",
            Self::Preview => "24",
            Self::Final => "19",
        }
    }

    fn nvenc_preset(self) -> &'static str {
        match self {
            Self::Proxy => "p2",
            Self::Preview => "p3",
            Self::Final => "p5",
        }
    }

    fn nvenc_cq(self) -> &'static str {
        match self {
            Self::Proxy => "29",
            Self::Preview => "24",
            Self::Final => "19",
        }
    }

    fn audio_bitrate(self) -> &'static str {
        match self {
            Self::Proxy => "96k",
            Self::Preview => "128k",
            Self::Final => "192k",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortraitLayout {
    CenterCrop,
    Contain,
    BlurPad,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderWorkloadClass {
    Light,
    Medium,
    Heavy,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoder {
    H264Nvenc,
    Libx264,
    Image,
    /// No video stream at all. Named so an audio deliverable is never mistaken for a render that
    /// failed to select an encoder.
    AudioOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub output: PathBuf,
    pub encoder: VideoEncoder,
    pub emits_progress: bool,
}

impl RenderCommand {
    /// Builds a command without involving a shell. The child becomes leader of a
    /// dedicated process group, so cancellation can terminate FFmpeg and any
    /// helper processes it may spawn without signalling soundAr itself.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    pub fn display_summary(&self) -> String {
        format!(
            "{} ({} arguments; output {})",
            self.program.display(),
            self.args.len(),
            self.output.display()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderCommandPlan {
    pub profile: RenderProfile,
    pub workload_class: RenderWorkloadClass,
    pub primary: RenderCommand,
    pub software_fallback: Option<RenderCommand>,
}

impl RenderCommandPlan {
    pub fn command_after_failure(&self, stderr: &str) -> Option<&RenderCommand> {
        if self.primary.encoder == VideoEncoder::H264Nvenc && should_fallback_from_nvenc(stderr) {
            self.software_fallback.as_ref()
        } else {
            None
        }
    }
}

pub fn build_proxy_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
) -> Result<RenderCommandPlan, MediaError> {
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let (width, height) = profile.landscape_dimensions();
    let filter = format!(
        "scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1,fps={}",
        profile.frame_rate()
    );
    let mut common = base_video_arguments(&paths.input)?;
    common.extend([
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-map"),
        OsString::from("0:a:0?"),
        OsString::from("-vf"),
        OsString::from(filter),
    ]);
    Ok(build_h264_plan(paths, common, profile, h264_nvenc_runtime))
}

pub fn build_thumbnail_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    at_us: i64,
) -> Result<RenderCommandPlan, MediaError> {
    if at_us < 0 {
        return Err(MediaError::new(
            "invalid_thumbnail_time",
            "Thumbnail time may not be negative",
        ));
    }
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-nostdin"),
        OsString::from("-n"),
        OsString::from("-ss"),
        OsString::from(format_timestamp_us(at_us)),
    ];
    args.extend(local_media_input_args(&paths.input)?);
    args.extend([
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-frames:v"),
        OsString::from("1"),
        OsString::from("-vf"),
        OsString::from(
            "scale=w=640:h=360:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1",
        ),
        OsString::from("-q:v"),
        OsString::from("3"),
        OsString::from("-f"),
        OsString::from("image2"),
        paths.output.as_os_str().to_os_string(),
    ]);
    Ok(RenderCommandPlan {
        profile: RenderProfile::Proxy,
        workload_class: RenderWorkloadClass::Light,
        primary: RenderCommand {
            program: paths.ffmpeg,
            args,
            output: paths.output,
            encoder: VideoEncoder::Image,
            emits_progress: false,
        },
        software_fallback: None,
    })
}

pub fn build_waveform_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    width: u32,
    height: u32,
) -> Result<RenderCommandPlan, MediaError> {
    if !(64..=8_192).contains(&width) || !(32..=4_096).contains(&height) {
        return Err(MediaError::new(
            "invalid_waveform_size",
            "Waveform dimensions are outside the supported range",
        ));
    }
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let filter = format!(
        "[0:a:0]aformat=channel_layouts=mono,showwavespic=s={width}x{height}:colors=0x6b7280[wave]"
    );
    let mut args = vec![
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-nostdin"),
        OsString::from("-n"),
    ];
    args.extend(local_media_input_args(&paths.input)?);
    args.extend([
        OsString::from("-filter_complex"),
        OsString::from(filter),
        OsString::from("-map"),
        OsString::from("[wave]"),
        OsString::from("-frames:v"),
        OsString::from("1"),
        OsString::from("-f"),
        OsString::from("image2"),
        paths.output.as_os_str().to_os_string(),
    ]);
    Ok(RenderCommandPlan {
        profile: RenderProfile::Proxy,
        workload_class: RenderWorkloadClass::Light,
        primary: RenderCommand {
            program: paths.ffmpeg,
            args,
            output: paths.output,
            encoder: VideoEncoder::Image,
            emits_progress: false,
        },
        software_fallback: None,
    })
}

pub fn build_portrait_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
) -> Result<RenderCommandPlan, MediaError> {
    build_portrait_command_with_layout(
        ffmpeg,
        input,
        output,
        profile,
        h264_nvenc_runtime,
        PortraitLayout::CenterCrop,
    )
}

pub fn build_portrait_command_with_layout(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
    layout: PortraitLayout,
) -> Result<RenderCommandPlan, MediaError> {
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let (width, height) = profile.portrait_dimensions();
    let fps = profile.frame_rate();
    let mut common = base_video_arguments(&paths.input)?;
    match layout {
        PortraitLayout::CenterCrop => {
            common.extend([
                OsString::from("-map"),
                OsString::from("0:v:0"),
                OsString::from("-map"),
                OsString::from("0:a:0?"),
                OsString::from("-vf"),
                OsString::from(format!(
                    "scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},setsar=1,fps={fps}"
                )),
            ]);
        }
        PortraitLayout::Contain => {
            common.extend([
                OsString::from("-map"),
                OsString::from("0:v:0"),
                OsString::from("-map"),
                OsString::from("0:a:0?"),
                OsString::from("-vf"),
                OsString::from(format!(
                    "scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=0x111111,setsar=1,fps={fps}"
                )),
            ]);
        }
        PortraitLayout::BlurPad => {
            common.extend([
                OsString::from("-filter_complex"),
                OsString::from(format!(
                    "[0:v:0]split=2[bgsrc][fgsrc];[bgsrc]scale=w={width}:h={height}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={width}:{height},boxblur=luma_radius=24:luma_power=1[bg];[fgsrc]scale=w={width}:h={height}:force_original_aspect_ratio=decrease:force_divisible_by=2[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2,setsar=1,fps={fps}[video]"
                )),
                OsString::from("-map"),
                OsString::from("[video]"),
                OsString::from("-map"),
                OsString::from("0:a:0?"),
            ]);
        }
    }
    Ok(build_h264_plan(paths, common, profile, h264_nvenc_runtime))
}

struct ValidatedPlanPaths {
    ffmpeg: PathBuf,
    input: PathBuf,
    output: PathBuf,
}

fn validate_plan_paths(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
) -> Result<ValidatedPlanPaths, MediaError> {
    let ffmpeg = fs::canonicalize(ffmpeg).map_err(|error| {
        MediaError::new("ffmpeg_unavailable", "FFmpeg could not be resolved")
            .detail(error.to_string())
    })?;
    if !is_executable_file(&ffmpeg) {
        return Err(MediaError::new(
            "ffmpeg_unavailable",
            "The configured FFmpeg path is not executable",
        ));
    }
    let input = fs::canonicalize(input).map_err(|error| {
        MediaError::new(
            "render_input_not_found",
            "The render input could not be opened",
        )
        .detail(error.to_string())
    })?;
    if !fs::metadata(&input)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return Err(MediaError::new(
            "invalid_render_input",
            "The render input is not a regular file",
        ));
    }
    let output_parent = output.parent().ok_or_else(|| {
        MediaError::new(
            "invalid_render_output",
            "The render output has no parent directory",
        )
    })?;
    let output_parent = fs::canonicalize(output_parent).map_err(|error| {
        MediaError::new(
            "invalid_render_output",
            "The render output directory could not be opened",
        )
        .detail(error.to_string())
    })?;
    if !output_parent.is_dir() {
        return Err(MediaError::new(
            "invalid_render_output",
            "The render output parent is not a directory",
        ));
    }
    let filename = output.file_name().ok_or_else(|| {
        MediaError::new("invalid_render_output", "The render output has no filename")
    })?;
    let output = output_parent.join(filename);
    if output == input {
        return Err(MediaError::new(
            "render_would_overwrite_source",
            "The render output may not overwrite its source",
        ));
    }
    if fs::symlink_metadata(&output).is_ok() {
        return Err(MediaError::new(
            "render_output_exists",
            "The render staging output already exists",
        ));
    }
    Ok(ValidatedPlanPaths {
        ffmpeg,
        input,
        output,
    })
}

/// Encode the episode as a podcast audio file, carrying its chapter marks.
///
/// M4A rather than MP3: chapter marks are a first-class part of the container, so a player reads
/// them without depending on an ID3 extension a given client may or may not honour.
///
/// The chapter document is a separate input mapped in explicitly. Chapters cannot be expressed as
/// command-line arguments, and building the document as text keeps a title containing FFmetadata
/// syntax from reaching the command line at all.
pub fn build_podcast_audio_command(
    ffmpeg: &Path,
    input: &Path,
    chapters_metadata: Option<&Path>,
    output: &Path,
) -> Result<RenderCommandPlan, MediaError> {
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let metadata = chapters_metadata
        .map(|path| {
            fs::canonicalize(path).map_err(|error| {
                MediaError::new(
                    "render_input_not_found",
                    "The chapter metadata document could not be opened",
                )
                .detail(error.to_string())
            })
        })
        .transpose()?;

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
    args.extend(local_media_input_args(&paths.input)?);
    if let Some(metadata) = &metadata {
        args.extend([
            OsString::from("-f"),
            OsString::from("ffmetadata"),
            OsString::from("-i"),
            metadata.as_os_str().to_os_string(),
        ]);
    }
    args.extend([
        OsString::from("-map"),
        OsString::from("0:a:0"),
        // Metadata comes from the chapter document when there is one, and from nowhere otherwise.
        // Carrying the master's own metadata through would publish whatever the render left on it.
        OsString::from("-map_metadata"),
        OsString::from(if metadata.is_some() { "1" } else { "-1" }),
        OsString::from("-map_chapters"),
        OsString::from(if metadata.is_some() { "1" } else { "-1" }),
        OsString::from("-c:a"),
        OsString::from("aac"),
        OsString::from("-b:a"),
        OsString::from("160k"),
        OsString::from("-ar"),
        OsString::from("48000"),
        OsString::from("-movflags"),
        OsString::from("+faststart"),
        OsString::from("-f"),
        OsString::from("mp4"),
    ]);
    args.push(paths.output.as_os_str().to_os_string());

    Ok(RenderCommandPlan {
        profile: RenderProfile::Final,
        // Audio-only transcoding never touches the GPU and should not queue behind rendering.
        workload_class: RenderWorkloadClass::Light,
        primary: RenderCommand {
            program: paths.ffmpeg,
            args,
            output: paths.output,
            encoder: VideoEncoder::AudioOnly,
            emits_progress: true,
        },
        software_fallback: None,
    })
}

/// Measure a master's loudness without producing a file.
///
/// `loudnorm` in analysis mode writes its measurement to stderr and decodes to null, so this
/// reads the same numbers a platform's own normalizer will read rather than approximating them.
pub fn build_loudness_analysis_command(
    ffmpeg: &Path,
    input: &Path,
) -> Result<RenderCommand, MediaError> {
    let ffmpeg = fs::canonicalize(ffmpeg).map_err(|error| {
        MediaError::new("ffmpeg_unavailable", "FFmpeg could not be resolved")
            .detail(error.to_string())
    })?;
    if !is_executable_file(&ffmpeg) {
        return Err(MediaError::new(
            "ffmpeg_unavailable",
            "The configured FFmpeg path is not executable",
        ));
    }
    let input = fs::canonicalize(input).map_err(|error| {
        MediaError::new(
            "render_input_not_found",
            "The measured media could not be opened",
        )
        .detail(error.to_string())
    })?;

    let mut args = vec![OsString::from("-hide_banner"), OsString::from("-nostdin")];
    args.extend(local_media_input_args(&input)?);
    args.extend([
        OsString::from("-map"),
        OsString::from("0:a:0"),
        OsString::from("-af"),
        OsString::from("loudnorm=print_format=json"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ]);
    Ok(RenderCommand {
        program: ffmpeg,
        args,
        // Analysis writes no file; the measurement is the output.
        output: PathBuf::from("/dev/null"),
        encoder: VideoEncoder::AudioOnly,
        emits_progress: false,
    })
}

/// Cut a short vertical trailer out of the finished master.
///
/// The range is placed before the input so FFmpeg seeks rather than decoding to the cut point,
/// which matters because a trailer is usually taken from the middle of a long episode. The frame is
/// scaled to cover and then centre-cropped, so a landscape master yields a portrait trailer without
/// letterboxing.
pub fn build_trailer_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    start_us: i64,
    end_us: i64,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
) -> Result<RenderCommandPlan, MediaError> {
    if start_us < 0 || end_us <= start_us {
        return Err(MediaError::new(
            "invalid_trailer_range",
            "A trailer must have a positive range inside the master",
        ));
    }
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    let (width, height) = profile.portrait_dimensions();
    let filter = format!(
        "scale=w={width}:h={height}:force_original_aspect_ratio=increase,crop={width}:{height},setsar=1,fps={}",
        profile.frame_rate()
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
        OsString::from("-ss"),
        OsString::from(format_timestamp_us(start_us)),
        OsString::from("-to"),
        OsString::from(format_timestamp_us(end_us)),
    ];
    common.extend(local_media_input_args(&paths.input)?);
    common.extend([
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-map_chapters"),
        OsString::from("-1"),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-map"),
        OsString::from("0:a:0?"),
        OsString::from("-vf"),
        OsString::from(filter),
    ]);
    Ok(build_h264_plan(paths, common, profile, h264_nvenc_runtime))
}

/// Render a square audiogram from the episode's audio.
///
/// An audiogram is audio that can be posted where only video plays. The waveform is drawn from the
/// audio itself rather than from a stored waveform image, so what a viewer sees is what they are
/// hearing at that moment.
pub fn build_audiogram_command(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
) -> Result<RenderCommandPlan, MediaError> {
    let paths = validate_plan_paths(ffmpeg, input, output)?;
    // Square, because that is the shape that survives every feed without being cropped.
    let size = profile.portrait_dimensions().0;
    let rate = profile.frame_rate();
    let filter = format!(
        "color=c=0x18181B:s={size}x{size}:r={rate}[bg];\
[0:a]showwaves=s={size}x{half}:mode=cline:rate={rate}:colors=0xF5F5F4[wave];\
[bg][wave]overlay=x=0:y=(H-h)/2:shortest=1,format=yuv420p[v]",
        half = size / 2
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
    ];
    common.extend(local_media_input_args(&paths.input)?);
    common.extend([
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-map_chapters"),
        OsString::from("-1"),
        OsString::from("-filter_complex"),
        OsString::from(filter),
        OsString::from("-map"),
        OsString::from("[v]"),
        OsString::from("-map"),
        OsString::from("0:a:0"),
    ]);
    Ok(build_h264_plan(paths, common, profile, h264_nvenc_runtime))
}

fn base_video_arguments(input: &Path) -> Result<Vec<OsString>, MediaError> {
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
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-map_chapters"),
        OsString::from("-1"),
    ]);
    Ok(args)
}

fn build_h264_plan(
    paths: ValidatedPlanPaths,
    common: Vec<OsString>,
    profile: RenderProfile,
    h264_nvenc_runtime: bool,
) -> RenderCommandPlan {
    let build = |encoder: VideoEncoder| {
        let mut args = common.clone();
        args.extend(encoder_arguments(encoder, profile));
        args.extend([
            OsString::from("-c:a"),
            OsString::from("aac"),
            OsString::from("-b:a"),
            OsString::from(profile.audio_bitrate()),
            OsString::from("-ar"),
            OsString::from("48000"),
            OsString::from("-movflags"),
            OsString::from("+faststart"),
            OsString::from("-f"),
            OsString::from("mp4"),
        ]);
        args.push(paths.output.as_os_str().to_os_string());
        RenderCommand {
            program: paths.ffmpeg.clone(),
            args,
            output: paths.output.clone(),
            encoder,
            emits_progress: true,
        }
    };
    let workload_class = match profile {
        RenderProfile::Proxy => RenderWorkloadClass::Medium,
        RenderProfile::Preview => RenderWorkloadClass::Medium,
        RenderProfile::Final => RenderWorkloadClass::Heavy,
    };
    if h264_nvenc_runtime {
        RenderCommandPlan {
            profile,
            workload_class,
            primary: build(VideoEncoder::H264Nvenc),
            software_fallback: Some(build(VideoEncoder::Libx264)),
        }
    } else {
        RenderCommandPlan {
            profile,
            workload_class,
            primary: build(VideoEncoder::Libx264),
            software_fallback: None,
        }
    }
}

fn encoder_arguments(encoder: VideoEncoder, profile: RenderProfile) -> Vec<OsString> {
    match encoder {
        VideoEncoder::H264Nvenc => vec![
            OsString::from("-c:v"),
            OsString::from("h264_nvenc"),
            OsString::from("-preset"),
            OsString::from(profile.nvenc_preset()),
            OsString::from("-tune"),
            OsString::from("hq"),
            OsString::from("-rc"),
            OsString::from("vbr"),
            OsString::from("-cq"),
            OsString::from(profile.nvenc_cq()),
            OsString::from("-b:v"),
            OsString::from("0"),
            OsString::from("-pix_fmt"),
            OsString::from("yuv420p"),
        ],
        VideoEncoder::Libx264 => vec![
            OsString::from("-c:v"),
            OsString::from("libx264"),
            OsString::from("-preset"),
            OsString::from(profile.software_preset()),
            OsString::from("-crf"),
            OsString::from(profile.software_crf()),
            OsString::from("-pix_fmt"),
            OsString::from("yuv420p"),
        ],
        // Neither shape carries video encoder settings: one writes a still, the other no picture.
        VideoEncoder::Image | VideoEncoder::AudioOnly => Vec::new(),
    }
}

pub fn should_fallback_from_nvenc(stderr: &str) -> bool {
    let diagnostic = stderr.to_ascii_lowercase();
    [
        "cannot load libcuda",
        "cannot init cuda",
        "no nvenc capable devices",
        "unsupported device",
        "driver does not support",
        "openencodesessionex failed",
        "failed setup for format cuda",
        "nvenc.*initialization failed",
        "resource temporarily unavailable",
        "out of memory",
    ]
    .iter()
    .any(|needle| {
        if *needle == "nvenc.*initialization failed" {
            diagnostic.contains("nvenc") && diagnostic.contains("initialization failed")
        } else {
            diagnostic.contains(needle)
        }
    })
}

pub fn terminate_process_group(child: &mut Child, grace: Duration) -> Result<(), MediaError> {
    if child
        .try_wait()
        .map_err(|error| {
            MediaError::new(
                "render_cancel_failed",
                "The render process could not be inspected",
            )
            .detail(error.to_string())
        })?
        .is_some()
    {
        return Ok(());
    }
    let process_group = -(child.id() as i32);
    let term_result = unsafe { libc::kill(process_group, libc::SIGTERM) };
    if term_result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(MediaError::new(
                "render_cancel_failed",
                "The render process group could not be stopped",
            )
            .detail(error.to_string()));
        }
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| {
                MediaError::new(
                    "render_cancel_failed",
                    "The cancelled render process could not be monitored",
                )
                .detail(error.to_string())
            })?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    unsafe {
        libc::kill(process_group, libc::SIGKILL);
    }
    child.wait().map_err(|error| {
        MediaError::new(
            "render_cancel_failed",
            "The cancelled render process could not be reaped",
        )
        .detail(error.to_string())
    })?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FfmpegProgressPhase {
    Continue,
    End,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FfmpegProgress {
    pub frame: Option<u64>,
    pub fps: Option<f64>,
    pub bitrate_kbps: Option<f64>,
    pub total_size_bytes: Option<u64>,
    pub out_time_us: Option<i64>,
    pub speed: Option<f64>,
    pub fraction: Option<f64>,
    pub phase: FfmpegProgressPhase,
}

pub fn parse_ffmpeg_progress(text: &str, expected_duration_us: Option<i64>) -> Vec<FfmpegProgress> {
    let mut records = Vec::new();
    let mut fields = BTreeMap::<String, String>::new();
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        fields.insert(key.to_string(), value.trim().to_string());
        if key == "progress" {
            if let Some(record) = progress_from_fields(&fields, expected_duration_us) {
                records.push(record);
            }
            fields.clear();
        }
    }
    records
}

#[derive(Default)]
pub struct FfmpegProgressParser {
    pending: String,
    fields: BTreeMap<String, String>,
}

impl FfmpegProgressParser {
    pub fn push(&mut self, chunk: &[u8], expected_duration_us: Option<i64>) -> Vec<FfmpegProgress> {
        self.pending.push_str(&String::from_utf8_lossy(chunk));
        let mut records = Vec::new();
        while let Some(newline) = self.pending.find('\n') {
            let line = self.pending[..newline].trim_end_matches('\r').to_string();
            self.pending.drain(..=newline);
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            if key.is_empty()
                || !key
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                continue;
            }
            self.fields
                .insert(key.to_string(), value.trim().to_string());
            if key == "progress" {
                if let Some(record) = progress_from_fields(&self.fields, expected_duration_us) {
                    records.push(record);
                }
                self.fields.clear();
            }
        }
        records
    }
}

fn progress_from_fields(
    fields: &BTreeMap<String, String>,
    expected_duration_us: Option<i64>,
) -> Option<FfmpegProgress> {
    let phase = match fields.get("progress")?.as_str() {
        "continue" => FfmpegProgressPhase::Continue,
        "end" => FfmpegProgressPhase::End,
        _ => return None,
    };
    let out_time_us = fields
        .get("out_time_us")
        .or_else(|| fields.get("out_time_ms"))
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            fields
                .get("out_time")
                .and_then(|value| parse_clock_time_us(value))
        });
    let fraction = if phase == FfmpegProgressPhase::End {
        expected_duration_us
            .filter(|duration| *duration > 0)
            .map(|_| 1.0)
    } else {
        out_time_us
            .zip(expected_duration_us)
            .and_then(|(current, duration)| {
                (duration > 0).then_some((current as f64 / duration as f64).clamp(0.0, 1.0))
            })
    };
    Some(FfmpegProgress {
        frame: fields.get("frame").and_then(|value| value.parse().ok()),
        fps: fields.get("fps").and_then(|value| value.parse().ok()),
        bitrate_kbps: fields
            .get("bitrate")
            .and_then(|value| value.strip_suffix("kbits/s"))
            .and_then(|value| value.trim().parse().ok()),
        total_size_bytes: fields
            .get("total_size")
            .and_then(|value| value.parse().ok()),
        out_time_us,
        speed: fields
            .get("speed")
            .and_then(|value| value.strip_suffix('x'))
            .and_then(|value| value.trim().parse().ok()),
        fraction,
        phase,
    })
}

fn parse_clock_time_us(value: &str) -> Option<i64> {
    let mut parts = value.trim().split(':');
    let hours = parts.next()?.parse::<i64>().ok()?;
    let minutes = parts.next()?.parse::<i64>().ok()?;
    let seconds = parts.next()?;
    if parts.next().is_some() || hours < 0 || !(0..60).contains(&minutes) {
        return None;
    }
    let (whole_seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let whole_seconds = whole_seconds.parse::<i64>().ok()?;
    if !(0..60).contains(&whole_seconds)
        || !fraction.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let mut micros = fraction.chars().take(6).collect::<String>();
    while micros.len() < 6 {
        micros.push('0');
    }
    let micros = micros.parse::<i64>().ok()?;
    hours
        .checked_mul(3_600_000_000)?
        .checked_add(minutes.checked_mul(60_000_000)?)?
        .checked_add(whole_seconds.checked_mul(1_000_000)?)?
        .checked_add(micros)
}

fn format_timestamp_us(value: i64) -> String {
    format!("{}.{:06}", value / 1_000_000, value.rem_euclid(1_000_000))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PublishedArtifact {
    pub path: PathBuf,
    pub size_bytes: u64,
}

pub fn sibling_staging_path(final_path: &Path) -> Result<PathBuf, MediaError> {
    let parent = final_path.parent().ok_or_else(|| {
        MediaError::new(
            "invalid_publication_path",
            "The final artifact has no parent directory",
        )
    })?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        MediaError::new(
            "invalid_publication_path",
            "The artifact directory could not be opened",
        )
        .detail(error.to_string())
    })?;
    if !parent.is_dir() {
        return Err(MediaError::new(
            "invalid_publication_path",
            "The artifact parent is not a directory",
        ));
    }
    let filename = final_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            MediaError::new(
                "invalid_publication_path",
                "The final artifact filename must be valid UTF-8",
            )
        })?;
    Ok(parent.join(format!(".{filename}.{}.partial", Uuid::new_v4().simple())))
}

pub fn publish_atomic<F>(
    staging: &Path,
    final_path: &Path,
    validate: F,
) -> Result<PublishedArtifact, MediaError>
where
    F: FnOnce(&Path) -> Result<(), MediaError>,
{
    let staging_parent = staging.parent().ok_or_else(|| {
        MediaError::new(
            "invalid_publication_path",
            "The staging artifact has no parent directory",
        )
    })?;
    let final_parent = final_path.parent().ok_or_else(|| {
        MediaError::new(
            "invalid_publication_path",
            "The final artifact has no parent directory",
        )
    })?;
    let staging_parent = fs::canonicalize(staging_parent).map_err(|error| {
        MediaError::new(
            "invalid_publication_path",
            "The staging artifact directory could not be opened",
        )
        .detail(error.to_string())
    })?;
    let final_parent = fs::canonicalize(final_parent).map_err(|error| {
        MediaError::new(
            "invalid_publication_path",
            "The final artifact directory could not be opened",
        )
        .detail(error.to_string())
    })?;
    if staging_parent != final_parent {
        return Err(MediaError::new(
            "non_atomic_publication",
            "Staging and final artifacts must be siblings on the same filesystem",
        ));
    }
    let staging_filename = staging.file_name().ok_or_else(|| {
        MediaError::new(
            "invalid_publication_path",
            "The staging artifact has no filename",
        )
    })?;
    let final_filename = final_path.file_name().ok_or_else(|| {
        MediaError::new(
            "invalid_publication_path",
            "The final artifact has no filename",
        )
    })?;
    let staging = staging_parent.join(staging_filename);
    let final_path = final_parent.join(final_filename);
    if staging == final_path {
        return Err(MediaError::new(
            "invalid_publication_path",
            "The staging and final artifacts must be different files",
        ));
    }
    let staging_metadata = fs::symlink_metadata(&staging).map_err(|error| {
        MediaError::new(
            "staging_artifact_missing",
            "The staged artifact could not be opened",
        )
        .detail(error.to_string())
    })?;
    if !staging_metadata.file_type().is_file() || staging_metadata.len() == 0 {
        return Err(MediaError::new(
            "invalid_staging_artifact",
            "The staged artifact is not a non-empty regular file",
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(&final_path) {
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            return Err(MediaError::new(
                "unsafe_publication_target",
                "The final artifact may not replace a symlink or directory",
            ));
        }
    }
    if let Err(error) = validate(&staging) {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    fs::File::open(&staging)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            MediaError::new(
                "publication_sync_failed",
                "The staged artifact could not be synchronized",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
    fs::rename(&staging, &final_path).map_err(|error| {
        MediaError::new(
            "publication_commit_failed",
            "The staged artifact could not be atomically published",
        )
        .detail(error.to_string())
        .retryable(true)
    })?;
    // fsync the directory entry so a successful publication survives a power
    // loss after rename on filesystems that support directory synchronization.
    fs::File::open(&final_parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            MediaError::new(
                "publication_sync_failed",
                "The artifact directory could not be synchronized",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
    let size_bytes = fs::metadata(&final_path)
        .map_err(|error| {
            MediaError::new(
                "publication_validation_failed",
                "The published artifact could not be inspected",
            )
            .detail(error.to_string())
        })?
        .len();
    Ok(PublishedArtifact {
        path: final_path,
        size_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::contracts::Microseconds;
    use crate::video::media::{is_executable_file, probe_media};
    use std::{
        env,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "soundar-renderer-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn proxy_plan_uses_arguments_not_a_shell_and_has_a_nvenc_fallback() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let root = TestDirectory::new("command-plan");
        let input = root.0.join("input $(touch should-not-run).mp4");
        fs::write(&input, b"fixture").expect("create input");
        let output = root.0.join("proxy.partial");
        let plan = build_proxy_command(&ffmpeg, &input, &output, RenderProfile::Proxy, true)
            .expect("build command");
        assert_eq!(plan.primary.encoder, VideoEncoder::H264Nvenc);
        assert_eq!(
            plan.software_fallback.as_ref().map(|value| value.encoder),
            Some(VideoEncoder::Libx264)
        );
        let canonical_input = fs::canonicalize(input).unwrap().into_os_string();
        assert!(plan.primary.args.contains(&canonical_input));
        assert!(plan.primary.args.contains(&OsString::from("h264_nvenc")));
        assert!(plan
            .software_fallback
            .as_ref()
            .unwrap()
            .args
            .contains(&OsString::from("libx264")));
        assert!(!root.0.join("should-not-run").exists());
    }

    /// Render a short A/V fixture so the deliverable commands are proved against real media
    /// rather than only against their own argument lists.
    fn fixture_media(ffmpeg: &Path, output: &Path, seconds: &str) -> bool {
        Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=size=640x360:rate=30:duration={seconds}"),
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={seconds}"),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(output)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn run_plan(plan: &RenderCommandPlan) -> bool {
        plan.primary
            .command()
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn a_podcast_deliverable_is_audio_only_and_carries_its_chapters() {
        let (Some(ffmpeg), Some(ffprobe)) = (find_in_path("ffmpeg"), find_in_path("ffprobe"))
        else {
            return;
        };
        let root = TestDirectory::new("podcast-audio");
        let master = root.0.join("master.mp4");
        assert!(
            fixture_media(&ffmpeg, &master, "4"),
            "the fixture master could not be rendered"
        );
        let chapters = root.0.join("chapters.txt");
        fs::write(
            &chapters,
            crate::video::release::ffmetadata_chapters(&[
                crate::video::release::ReleaseChapter {
                    id: "chapter-one".into(),
                    title: "The letter".into(),
                    start_us: Microseconds::ZERO,
                    end_us: Microseconds(2_000_000),
                },
                crate::video::release::ReleaseChapter {
                    id: "chapter-two".into(),
                    // Characters that are FFmetadata syntax must survive the round trip.
                    title: "Act 2; the reveal".into(),
                    start_us: Microseconds(2_000_000),
                    end_us: Microseconds(4_000_000),
                },
            ]),
        )
        .expect("write chapter metadata");

        let output = root.0.join("episode.m4a");
        let plan = build_podcast_audio_command(&ffmpeg, &master, Some(&chapters), &output)
            .expect("build podcast plan");
        assert_eq!(plan.primary.encoder, VideoEncoder::AudioOnly);
        assert!(run_plan(&plan), "podcast audio render failed");

        let probe = probe_media(&output, &ffprobe).expect("probe the podcast deliverable");
        assert!(
            probe.primary_video_stream.is_none(),
            "a podcast deliverable must carry no picture"
        );
        assert!(probe.primary_audio_stream.is_some());
        assert_eq!(probe.chapters.len(), 2);
        assert_eq!(probe.chapters[0].title.as_deref(), Some("The letter"));
        assert_eq!(
            probe.chapters[1].title.as_deref(),
            Some("Act 2; the reveal")
        );
        assert_eq!(probe.chapters[1].start_us, 2_000_000);
    }

    #[test]
    fn loudness_is_measured_from_the_media_rather_than_assumed() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let root = TestDirectory::new("loudness");
        let master = root.0.join("master.mp4");
        assert!(
            fixture_media(&ffmpeg, &master, "3"),
            "the fixture master could not be rendered"
        );

        let command = build_loudness_analysis_command(&ffmpeg, &master).expect("build analysis");
        let output = command.command().output().expect("run loudness analysis");
        assert!(output.status.success());
        // Analysis writes its measurement to stderr and produces no file.
        let measured = crate::video::quality::parse_loudness_analysis(&String::from_utf8_lossy(
            &output.stderr,
        ))
        .expect("a measurement from real media");
        // A 440 Hz tone is loud and well above silence; the exact value is the tone's, not ours.
        assert!(
            measured.integrated_lufs_milli > -40_000 && measured.integrated_lufs_milli < 0,
            "implausible integrated loudness: {}",
            measured.integrated_lufs_milli
        );
        assert!(measured.true_peak_db_milli < 12_000);
    }

    #[test]
    fn a_trailer_is_cut_to_its_range_and_is_vertical() {
        let (Some(ffmpeg), Some(ffprobe)) = (find_in_path("ffmpeg"), find_in_path("ffprobe"))
        else {
            return;
        };
        let root = TestDirectory::new("trailer");
        let master = root.0.join("master.mp4");
        assert!(
            fixture_media(&ffmpeg, &master, "6"),
            "the fixture master could not be rendered"
        );
        let output = root.0.join("trailer.mp4");
        let plan = build_trailer_command(
            &ffmpeg,
            &master,
            &output,
            2_000_000,
            5_000_000,
            RenderProfile::Proxy,
            false,
        )
        .expect("build trailer plan");
        assert!(run_plan(&plan), "trailer render failed");

        let probe = probe_media(&output, &ffprobe).expect("probe the trailer");
        let video = probe
            .streams
            .iter()
            .find(|stream| stream.codec_type == "video")
            .expect("the trailer has picture");
        // A landscape master yields a portrait trailer without letterboxing.
        assert_eq!((video.width, video.height), (Some(540), Some(960)));
        assert!(
            (probe.duration_us - 3_000_000).abs() < 400_000,
            "trailer ran {}us, expected about 3s",
            probe.duration_us
        );
    }

    #[test]
    fn a_trailer_range_must_be_positive_and_ordered() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let root = TestDirectory::new("trailer-range");
        let input = root.0.join("input.mp4");
        fs::write(&input, b"fixture").unwrap();
        for (start, end) in [(-1_000_000, 1_000_000), (2_000_000, 2_000_000)] {
            let error = build_trailer_command(
                &ffmpeg,
                &input,
                &root.0.join("out.mp4"),
                start,
                end,
                RenderProfile::Proxy,
                false,
            )
            .expect_err("an unusable range is refused");
            assert_eq!(error.code, "invalid_trailer_range");
        }
    }

    #[test]
    fn an_audiogram_is_square_and_carries_both_picture_and_sound() {
        let (Some(ffmpeg), Some(ffprobe)) = (find_in_path("ffmpeg"), find_in_path("ffprobe"))
        else {
            return;
        };
        let root = TestDirectory::new("audiogram");
        let master = root.0.join("master.mp4");
        assert!(
            fixture_media(&ffmpeg, &master, "3"),
            "the fixture master could not be rendered"
        );
        let output = root.0.join("audiogram.mp4");
        let plan = build_audiogram_command(&ffmpeg, &master, &output, RenderProfile::Proxy, false)
            .expect("build audiogram plan");
        assert!(run_plan(&plan), "audiogram render failed");

        let probe = probe_media(&output, &ffprobe).expect("probe the audiogram");
        let video = probe
            .streams
            .iter()
            .find(|stream| stream.codec_type == "video")
            .expect("the audiogram has picture");
        // Square, because that is the shape that survives every feed without being cropped.
        assert_eq!(video.width, video.height);
        assert_eq!(video.width, Some(540));
        assert!(probe.primary_audio_stream.is_some());
    }

    #[test]
    fn nvenc_fallback_only_handles_runtime_initialization_failures() {
        assert!(should_fallback_from_nvenc(
            "Cannot load libcuda.so.1; Nvenc initialization failed"
        ));
        assert!(should_fallback_from_nvenc("No NVENC capable devices found"));
        assert!(!should_fallback_from_nvenc(
            "Invalid data found when processing input"
        ));
    }

    #[test]
    fn thumbnail_and_waveform_plans_have_explicit_formats() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let root = TestDirectory::new("image-plans");
        let input = root.0.join("input.mp4");
        fs::write(&input, b"fixture").unwrap();
        let thumbnail =
            build_thumbnail_command(&ffmpeg, &input, &root.0.join("thumb.partial"), 2_345_678)
                .unwrap();
        assert!(thumbnail.primary.args.contains(&OsString::from("2.345678")));
        assert!(thumbnail.primary.args.contains(&OsString::from("image2")));
        let waveform =
            build_waveform_command(&ffmpeg, &input, &root.0.join("wave.partial"), 1200, 240)
                .unwrap();
        assert!(waveform.primary.args.iter().any(|argument| argument
            .to_string_lossy()
            .contains("showwavespic=s=1200x240")));
    }

    #[test]
    fn portrait_layouts_are_deterministic_and_bounded_by_profile() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let root = TestDirectory::new("portrait");
        let input = root.0.join("input.mp4");
        fs::write(&input, b"fixture").unwrap();
        let plan = build_portrait_command_with_layout(
            &ffmpeg,
            &input,
            &root.0.join("portrait.partial"),
            RenderProfile::Preview,
            false,
            PortraitLayout::BlurPad,
        )
        .unwrap();
        assert_eq!(plan.primary.encoder, VideoEncoder::Libx264);
        assert!(plan
            .primary
            .args
            .iter()
            .any(|argument| argument.to_string_lossy().contains("crop=720:1280")));
    }

    #[test]
    fn progress_parser_handles_multiple_records_and_partial_chunks() {
        let progress = "frame=12\nfps=30.0\nout_time_us=2500000\nspeed=2.0x\nprogress=continue\nframe=30\nout_time=00:00:05.000000\nprogress=end\n";
        let records = parse_ffmpeg_progress(progress, Some(5_000_000));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].fraction, Some(0.5));
        assert_eq!(records[0].speed, Some(2.0));
        assert_eq!(records[1].phase, FfmpegProgressPhase::End);
        assert_eq!(records[1].fraction, Some(1.0));

        let mut streaming = FfmpegProgressParser::default();
        assert!(streaming
            .push(b"frame=3\nout_time_us=10", Some(100))
            .is_empty());
        let emitted = streaming.push(b"0\nprogress=continue\n", Some(100));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].fraction, Some(1.0));
    }

    #[test]
    fn atomic_publication_validates_then_replaces_in_one_directory() {
        let root = TestDirectory::new("publication");
        let final_path = root.0.join("artifact.mp4");
        fs::write(&final_path, b"old").unwrap();
        let staging = sibling_staging_path(&final_path).unwrap();
        fs::write(&staging, b"new-media").unwrap();
        let receipt = publish_atomic(&staging, &final_path, |path| {
            (fs::metadata(path)
                .map_err(|error| {
                    MediaError::new("test_validation", "metadata failed").detail(error.to_string())
                })?
                .len()
                > 4)
            .then_some(())
            .ok_or_else(|| MediaError::new("test_validation", "too short"))
        })
        .expect("publish artifact");
        assert_eq!(
            receipt.path,
            fs::canonicalize(&root.0).unwrap().join("artifact.mp4")
        );
        assert_eq!(receipt.size_bytes, 9);
        assert_eq!(fs::read(final_path).unwrap(), b"new-media");
        assert!(!staging.exists());
    }

    #[test]
    fn failed_atomic_validation_removes_invalid_staging_and_preserves_final() {
        let root = TestDirectory::new("invalid-publication");
        let final_path = root.0.join("artifact.mp4");
        fs::write(&final_path, b"good").unwrap();
        let staging = sibling_staging_path(&final_path).unwrap();
        fs::write(&staging, b"bad").unwrap();
        let error = publish_atomic(&staging, &final_path, |_| {
            Err(MediaError::new("invalid_render", "validation failed"))
        })
        .unwrap_err();
        assert_eq!(error.code, "invalid_render");
        assert!(!staging.exists());
        assert_eq!(fs::read(final_path).unwrap(), b"good");
    }

    #[test]
    fn ffmpeg_proxy_and_publication_smoke_test_skips_when_tools_are_unavailable() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let Some(ffprobe) = find_in_path("ffprobe") else {
            return;
        };
        let root = TestDirectory::new("render-smoke");
        let source = root.0.join("source.mp4");
        let fixture_status = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=160x90:r=12:d=0.3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:sample_rate=48000:duration=0.3",
                "-shortest",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-y",
            ])
            .arg(&source)
            .status()
            .expect("generate local fixture");
        if !fixture_status.success() {
            return;
        }
        let final_path = root.0.join("proxy.mp4");
        let staging = sibling_staging_path(&final_path).unwrap();
        let plan =
            build_proxy_command(&ffmpeg, &source, &staging, RenderProfile::Proxy, false).unwrap();
        let status = plan.primary.command().status().expect("run proxy render");
        assert!(status.success());
        let receipt = publish_atomic(&staging, &final_path, |path| {
            probe_media(path, &ffprobe).map(|_| ())
        })
        .expect("publish valid proxy");
        let probe = probe_media(&receipt.path, &ffprobe).expect("probe published proxy");
        assert!(probe.duration_us > 0);
        assert!(probe
            .streams
            .iter()
            .any(|stream| stream.codec_type == "video"));
    }

    fn find_in_path(name: &str) -> Option<PathBuf> {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join(name))
            .find(|path| is_executable_file(path))
    }
}
