use super::renderer::ClipModelPaths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    env,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{Read as _, Write as _},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_PROBE_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEDIA_PROCESS_STDERR_BYTES: usize = 256 * 1024;
const MAX_TOOL_INSPECTION_BYTES: usize = 1024 * 1024;
const MAX_MEDIA_SOURCE_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_MANIFEST_SNIFF_BYTES: u64 = 64 * 1024;
/// Network-bearing and indirection demuxers are deliberately absent. Keep this list aligned
/// with ordinary, self-contained audio/video/image files supported by the studio.
pub const LOCAL_MEDIA_FORMAT_WHITELIST: &str = concat!(
    "aac,ac3,ac4,aiff,alaw,amr,ape,apng,asf,au,av1,avi,bmp_pipe,caf,",
    "dirac,dnxhd,dpx_pipe,dsf,dts,dtshd,dv,eac3,exr_pipe,f32be,f32le,",
    "f64be,f64le,flac,flv,g722,g723_1,g726,g726le,g728,g729,gif,gif_pipe,",
    "gsm,gxf,h261,h263,h264,hdr_pipe,hevc,iamf,iff,ilbc,ivf,j2k_pipe,",
    "jpeg_pipe,jpegls_pipe,jpegxl_anim,jpegxl_pipe,m4v,matroska,webm,mjpeg,",
    "mjpeg_2000,mlp,mlv,mmf,mov,mp4,m4a,3gp,3g2,mj2,mp3,mpc,mpc8,mpeg,",
    "mpegts,mpegtsraw,mpegvideo,mulaw,mxf,mxf_d10,mxf_opatom,nsv,nut,ogg,",
    "oma,opus,png_pipe,qoi_pipe,rawvideo,rm,roq,s16be,s16le,s24be,s24le,",
    "s32be,s32le,s8,sbc,shorten,smjpeg,sox,spdif,sunrast_pipe,svag,swf,",
    "tak,thp,tiff_pipe,truehd,tta,txd,u16be,u16le,u24be,u24le,u32be,u32le,",
    "u8,vc1,voc,w64,wav,wavarc,webp_pipe,wtv,wv,yuv4mpegpipe",
);
pub const LOCAL_MEDIA_PROTOCOL_WHITELIST: &str = "file";
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const TOOL_INSPECTION_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const PROCESS_PIPE_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_IMPORT_URL_BYTES: usize = 4_096;
const MAX_IMPORT_DNS_ADDRESSES: usize = 64;
const IMPORT_DNS_TIMEOUT: Duration = Duration::from_secs(8);
const IMPORT_DNS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_PROXY_CONNECT_HEADER_BYTES: usize = 16 * 1024;
const MAX_PROXY_CONNECTIONS: usize = 24;
const PROXY_HEADER_TIMEOUT: Duration = Duration::from_secs(8);
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const PROXY_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const PROXY_IO_POLL_TIMEOUT: Duration = Duration::from_millis(250);
const PROXY_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PROXY_RELAY_BUFFER_BYTES: usize = 32 * 1024;
const MAX_CAPTION_CUES: usize = 250_000;
const MAX_CAPTION_TEXT_BYTES: usize = 16_384;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MediaError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub retryable: bool,
}

impl MediaError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
            retryable: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(detail) = &self.detail {
            write!(formatter, "{}: {} ({detail})", self.code, self.message)
        } else {
            write!(formatter, "{}: {}", self.code, self.message)
        }
    }
}

impl std::error::Error for MediaError {}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MediaToolKind {
    Ffmpeg,
    Ffprobe,
    YtDlp,
    Node,
    Deno,
    FasterWhisper,
    WhisperCpp,
    /// stable-diffusion.cpp's CLI, which is how soundAr runs a local video-generation model.
    SdCli,
}

impl MediaToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
            Self::YtDlp => "yt_dlp",
            Self::Node => "node",
            Self::Deno => "deno",
            Self::FasterWhisper => "faster_whisper",
            Self::WhisperCpp => "whisper_cpp",
            Self::SdCli => "sd_cli",
        }
    }

    fn binary_names(self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["ffmpeg"],
            Self::Ffprobe => &["ffprobe"],
            Self::YtDlp => &["yt-dlp"],
            Self::Node => &["node"],
            Self::Deno => &["deno"],
            Self::FasterWhisper => &["faster-whisper", "faster_whisper"],
            Self::WhisperCpp => &["whisper-cli", "whisper-cpp"],
            Self::SdCli => &["sd-cli", "sd"],
        }
    }

    fn environment_keys(self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["SOUNDAR_FFMPEG_PATH", "SOUNDAR_FFMPEG_BIN", "FFMPEG_BIN"],
            Self::Ffprobe => &["SOUNDAR_FFPROBE_PATH", "SOUNDAR_FFPROBE_BIN", "FFPROBE_BIN"],
            Self::YtDlp => &["SOUNDAR_YT_DLP_PATH", "SOUNDAR_YT_DLP_BIN", "YT_DLP_BIN"],
            Self::Node => &["SOUNDAR_NODE_PATH", "SOUNDAR_NODE_BIN", "NODE_BIN"],
            Self::Deno => &["SOUNDAR_DENO_PATH", "SOUNDAR_DENO_BIN", "DENO_BIN"],
            Self::FasterWhisper => &[
                "SOUNDAR_FASTER_WHISPER_PATH",
                "SOUNDAR_FASTER_WHISPER_BIN",
                "FASTER_WHISPER_BIN",
            ],
            Self::WhisperCpp => &[
                "SOUNDAR_WHISPER_CPP_PATH",
                "SOUNDAR_WHISPER_CPP_BIN",
                "WHISPER_CPP_BIN",
                "WHISPER_CLI_BIN",
            ],
            Self::SdCli => &["SOUNDAR_SD_CLI_PATH", "SOUNDAR_SD_CLI_BIN", "SD_CLI_BIN"],
        }
    }

    fn setup_summary(self) -> &'static str {
        match self {
            Self::Ffmpeg | Self::Ffprobe => {
                "Install FFmpeg and FFprobe, then restart soundAr or configure their executable paths."
            }
            Self::YtDlp => {
                "Install a current yt-dlp release and yt-dlp-ejs for authorized single-link imports."
            }
            Self::Node | Self::Deno => {
                "Install a yt-dlp-supported JavaScript runtime (Deno or Node) for YouTube extraction."
            }
            Self::SdCli => {
                "Install stable-diffusion.cpp's sd-cli, built with CUDA, to generate video clips locally."
            }
            Self::FasterWhisper => {
                "Install faster-whisper in an isolated CUDA-capable runtime and configure its executable path."
            }
            Self::WhisperCpp => {
                "Install whisper.cpp and configure the whisper-cli executable as the CPU fallback."
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MediaToolStatus {
    pub kind: MediaToolKind,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_action: Option<String>,
    pub configured: bool,
}

impl MediaToolStatus {
    fn missing(kind: MediaToolKind, configured: bool, diagnostic: Option<String>) -> Self {
        Self {
            kind,
            available: false,
            path: None,
            version: None,
            capabilities: Vec::new(),
            diagnostic,
            setup_action: Some(kind.setup_summary().to_string()),
            configured,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MediaRuntimeStatus {
    pub ffmpeg: MediaToolStatus,
    pub ffprobe: MediaToolStatus,
    pub yt_dlp: MediaToolStatus,
    pub node: MediaToolStatus,
    pub deno: MediaToolStatus,
    pub faster_whisper: MediaToolStatus,
    pub whisper_cpp: MediaToolStatus,
    pub sd_cli: MediaToolStatus,
    pub ready_for_local_media: bool,
    pub ready_for_link_import: bool,
    pub ready_for_transcription: bool,
    pub h264_nvenc_compiled: bool,
    pub h264_nvenc_runtime: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_actions: Vec<String>,
}

impl MediaRuntimeStatus {
    pub fn tool(&self, kind: MediaToolKind) -> &MediaToolStatus {
        match kind {
            MediaToolKind::Ffmpeg => &self.ffmpeg,
            MediaToolKind::Ffprobe => &self.ffprobe,
            MediaToolKind::YtDlp => &self.yt_dlp,
            MediaToolKind::Node => &self.node,
            MediaToolKind::Deno => &self.deno,
            MediaToolKind::FasterWhisper => &self.faster_whisper,
            MediaToolKind::WhisperCpp => &self.whisper_cpp,
            MediaToolKind::SdCli => &self.sd_cli,
        }
    }
}

#[derive(Clone, Debug)]
struct DiscoveryContext {
    path: Option<OsString>,
    home: Option<PathBuf>,
    overrides: BTreeMap<MediaToolKind, PathBuf>,
    run_nvenc_smoke: bool,
}

impl DiscoveryContext {
    fn from_environment() -> Self {
        let mut overrides = BTreeMap::new();
        for kind in all_tool_kinds() {
            if let Some(path) = kind
                .environment_keys()
                .iter()
                .find_map(|key| env::var_os(key))
            {
                overrides.insert(kind, PathBuf::from(path));
            }
        }
        Self {
            path: env::var_os("PATH"),
            home: env::var_os("HOME").map(PathBuf::from),
            overrides,
            run_nvenc_smoke: true,
        }
    }
}

fn all_tool_kinds() -> [MediaToolKind; 8] {
    [
        MediaToolKind::Ffmpeg,
        MediaToolKind::Ffprobe,
        MediaToolKind::YtDlp,
        MediaToolKind::Node,
        MediaToolKind::Deno,
        MediaToolKind::FasterWhisper,
        MediaToolKind::WhisperCpp,
        MediaToolKind::SdCli,
    ]
}

pub fn discover_media_runtime() -> MediaRuntimeStatus {
    discover_media_runtime_with(&DiscoveryContext::from_environment())
}

fn discover_media_runtime_with(context: &DiscoveryContext) -> MediaRuntimeStatus {
    let mut tools = BTreeMap::new();
    for kind in all_tool_kinds() {
        tools.insert(kind, discover_tool(kind, context));
    }

    let mut ffmpeg = tools
        .remove(&MediaToolKind::Ffmpeg)
        .expect("every media tool is discovered");
    let ffprobe = tools
        .remove(&MediaToolKind::Ffprobe)
        .expect("every media tool is discovered");
    let yt_dlp = tools
        .remove(&MediaToolKind::YtDlp)
        .expect("every media tool is discovered");
    let node = tools
        .remove(&MediaToolKind::Node)
        .expect("every media tool is discovered");
    let deno = tools
        .remove(&MediaToolKind::Deno)
        .expect("every media tool is discovered");
    let faster_whisper = tools
        .remove(&MediaToolKind::FasterWhisper)
        .expect("every media tool is discovered");
    let whisper_cpp = tools
        .remove(&MediaToolKind::WhisperCpp)
        .expect("every media tool is discovered");

    let h264_nvenc_compiled = ffmpeg.capabilities.iter().any(|item| item == "h264_nvenc");
    let h264_nvenc_runtime = context.run_nvenc_smoke
        && h264_nvenc_compiled
        && ffmpeg.path.as_deref().is_some_and(probe_h264_nvenc_runtime);
    if h264_nvenc_runtime {
        ffmpeg.capabilities.push("h264_nvenc_runtime".to_string());
    }

    let ready_for_local_media = ffmpeg.available && ffprobe.available;
    let ready_for_link_import =
        ready_for_local_media && yt_dlp.available && (node.available || deno.available);
    let ready_for_transcription = faster_whisper.available || whisper_cpp.available;
    let mut setup_actions = Vec::new();
    if !ready_for_local_media {
        setup_actions.push(MediaToolKind::Ffmpeg.setup_summary().to_string());
    }
    if !yt_dlp.available {
        setup_actions.push(MediaToolKind::YtDlp.setup_summary().to_string());
    }
    if !node.available && !deno.available {
        setup_actions.push(MediaToolKind::Deno.setup_summary().to_string());
    }
    if !ready_for_transcription {
        setup_actions.push(MediaToolKind::FasterWhisper.setup_summary().to_string());
        setup_actions.push(MediaToolKind::WhisperCpp.setup_summary().to_string());
    }
    setup_actions.sort();
    setup_actions.dedup();

    let sd_cli = discover_tool(MediaToolKind::SdCli, context);

    MediaRuntimeStatus {
        ffmpeg,
        ffprobe,
        yt_dlp,
        node,
        deno,
        faster_whisper,
        whisper_cpp,
        sd_cli,
        ready_for_local_media,
        ready_for_link_import,
        ready_for_transcription,
        h264_nvenc_compiled,
        h264_nvenc_runtime,
        setup_actions,
    }
}

fn discover_tool(kind: MediaToolKind, context: &DiscoveryContext) -> MediaToolStatus {
    if let Some(configured) = context.overrides.get(&kind) {
        return validate_tool_candidate(kind, configured, true)
            .unwrap_or_else(|diagnostic| MediaToolStatus::missing(kind, true, Some(diagnostic)));
    }

    let mut candidates = Vec::new();
    if let Some(path) = &context.path {
        for directory in env::split_paths(path) {
            for name in kind.binary_names() {
                candidates.push(directory.join(name));
            }
        }
    }
    for directory in [
        "/usr/bin",
        "/usr/local/bin",
        "/opt/bin",
        "/snap/bin",
        "/var/lib/snapd/snap/bin",
        "/var/lib/flatpak/exports/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        for name in kind.binary_names() {
            candidates.push(Path::new(directory).join(name));
        }
    }
    if let Some(home) = &context.home {
        for directory in [
            ".local/bin",
            ".cargo/bin",
            ".npm-global/bin",
            ".volta/bin",
            ".asdf/shims",
            ".local/share/mise/shims",
            ".local/share/pnpm",
            ".bun/bin",
            ".linuxbrew/bin",
        ] {
            for name in kind.binary_names() {
                candidates.push(home.join(directory).join(name));
            }
        }

        let roots = match kind {
            MediaToolKind::Node => vec![
                home.join(".nvm/versions/node"),
                home.join(".local/share/fnm/node-versions"),
                home.join(".asdf/installs/nodejs"),
            ],
            MediaToolKind::YtDlp => vec![
                home.join(".local/share/soundar/runtimes"),
                home.join(".local/share/soundAr/runtimes"),
                home.join(".soundAr/runtimes"),
                home.join(".virtualenvs"),
            ],
            MediaToolKind::Deno => vec![home.join(".deno/bin"), home.join(".asdf/installs/deno")],
            MediaToolKind::FasterWhisper => vec![
                home.join(".local/share/soundar"),
                home.join(".cache/soundar"),
                home.join(".virtualenvs"),
            ],
            MediaToolKind::WhisperCpp => vec![
                home.join(".local/share/whisper.cpp"),
                home.join("whisper.cpp"),
            ],
            _ => Vec::new(),
        };
        for root in roots {
            collect_named_binaries(&root, kind.binary_names(), 6, 4_096, &mut candidates);
        }
    }
    if matches!(
        kind,
        MediaToolKind::WhisperCpp | MediaToolKind::FasterWhisper
    ) {
        for root in [
            PathBuf::from("/opt/soundar"),
            PathBuf::from("/opt/whisper.cpp"),
            PathBuf::from("/usr/local/share/whisper.cpp"),
        ] {
            collect_named_binaries(&root, kind.binary_names(), 5, 2_048, &mut candidates);
        }
    }

    let mut seen = HashSet::new();
    let mut statuses = candidates
        .into_iter()
        .filter_map(|candidate| {
            let canonical = fs::canonicalize(candidate).ok()?;
            if !seen.insert(canonical.clone()) {
                return None;
            }
            validate_tool_candidate(kind, &canonical, false).ok()
        })
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| {
        version_key(right.version.as_deref()).cmp(&version_key(left.version.as_deref()))
    });
    statuses.into_iter().next().unwrap_or_else(|| {
        MediaToolStatus::missing(
            kind,
            false,
            Some(format!(
                "No executable {} candidate passed its version check",
                kind.as_str()
            )),
        )
    })
}

fn validate_tool_candidate(
    kind: MediaToolKind,
    candidate: &Path,
    configured: bool,
) -> Result<MediaToolStatus, String> {
    let canonical = fs::canonicalize(candidate).map_err(|error| {
        format!(
            "Configured {} path {} could not be resolved: {error}",
            kind.as_str(),
            candidate.display()
        )
    })?;
    if !is_executable_file(&canonical) {
        return Err(format!(
            "{} is not a regular executable file",
            canonical.display()
        ));
    }

    let (version, mut capabilities) = inspect_tool(kind, &canonical)?;
    capabilities.sort();
    capabilities.dedup();
    Ok(MediaToolStatus {
        kind,
        available: true,
        path: Some(canonical),
        version: Some(version),
        capabilities,
        diagnostic: None,
        setup_action: None,
        configured,
    })
}

fn inspect_tool(kind: MediaToolKind, path: &Path) -> Result<(String, Vec<String>), String> {
    let version_args: &[&str] = match kind {
        MediaToolKind::Ffmpeg | MediaToolKind::Ffprobe => &["-version"],
        MediaToolKind::WhisperCpp => &["--version"],
        MediaToolKind::SdCli => &["--help"],
        _ => &["--version"],
    };
    let mut output = run_tool_inspection(path, version_args)?;
    if !output.status_success && kind == MediaToolKind::WhisperCpp {
        output = run_tool_inspection(path, &["--help"])?;
    }
    if !output.status_success {
        return Err(format!(
            "{} version check exited unsuccessfully",
            path.display()
        ));
    }
    if output.stdout_truncated || output.stderr_truncated {
        return Err(format!(
            "{} version check exceeded the {} byte output safety limit",
            path.display(),
            MAX_TOOL_INSPECTION_BYTES
        ));
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let first_line = combined
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if first_line.is_empty() || !version_identity_matches(kind, first_line, path) {
        return Err(format!(
            "{} did not identify itself as {}",
            path.display(),
            kind.as_str()
        ));
    }
    let version = first_line.chars().take(300).collect::<String>();
    let capabilities = if kind == MediaToolKind::Ffmpeg {
        inspect_ffmpeg_capabilities(path)
    } else {
        Vec::new()
    };
    Ok((version, capabilities))
}

fn run_tool_inspection(path: &Path, args: &[&str]) -> Result<BoundedCommandOutput, String> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_bounded_command_with_limits(
        path,
        &args,
        TOOL_INSPECTION_TIMEOUT,
        MAX_TOOL_INSPECTION_BYTES,
        MAX_TOOL_INSPECTION_BYTES,
    )
    .map_err(|error| format!("Could not inspect {}: {error}", path.display()))
}

fn version_identity_matches(kind: MediaToolKind, first_line: &str, path: &Path) -> bool {
    let value = first_line.to_ascii_lowercase();
    match kind {
        MediaToolKind::Ffmpeg => value.contains("ffmpeg version"),
        MediaToolKind::Ffprobe => value.contains("ffprobe version"),
        MediaToolKind::YtDlp => value.chars().any(|character| character.is_ascii_digit()),
        MediaToolKind::Node => {
            value.trim_start().starts_with('v')
                && value.chars().any(|character| character.is_ascii_digit())
        }
        MediaToolKind::Deno => value.contains("deno"),
        // sd-cli identifies itself through its usage banner rather than a version string.
        MediaToolKind::SdCli => {
            value.contains("usage")
                || value.contains("sd")
                || path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with("sd"))
        }
        MediaToolKind::FasterWhisper => {
            value.contains("faster")
                || path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.contains("faster"))
        }
        MediaToolKind::WhisperCpp => {
            value.contains("whisper")
                || path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.contains("whisper"))
        }
    }
}

fn inspect_ffmpeg_capabilities(ffmpeg: &Path) -> Vec<String> {
    let mut capabilities = Vec::new();
    let encoders = bounded_tool_stdout(ffmpeg, &["-hide_banner", "-encoders"]);
    for encoder in [
        "h264_nvenc",
        "hevc_nvenc",
        "av1_nvenc",
        "libx264",
        "libx265",
    ] {
        if contains_capability_line(&encoders, encoder) {
            capabilities.push(encoder.to_string());
        }
    }
    let filters = bounded_tool_stdout(ffmpeg, &["-hide_banner", "-filters"]);
    for filter in [
        "subtitles",
        "drawtext",
        "showwavespic",
        "loudnorm",
        "rubberband",
    ] {
        if contains_capability_line(&filters, filter) {
            capabilities.push(filter.to_string());
        }
    }
    let hardware = bounded_tool_stdout(ffmpeg, &["-hide_banner", "-hwaccels"]);
    for accelerator in ["cuda", "vaapi", "qsv", "vulkan"] {
        if hardware.lines().any(|line| line.trim() == accelerator) {
            capabilities.push(format!("hwaccel_{accelerator}"));
        }
    }
    capabilities
}

fn bounded_tool_stdout(program: &Path, args: &[&str]) -> String {
    run_tool_inspection(program, args)
        .ok()
        .filter(|output| {
            output.status_success && !output.stdout_truncated && !output.stderr_truncated
        })
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

fn contains_capability_line(output: &str, capability: &str) -> bool {
    output.lines().any(|line| {
        line.split_ascii_whitespace()
            .any(|field| field == capability)
    })
}

pub fn probe_h264_nvenc_runtime(ffmpeg: &Path) -> bool {
    if !is_executable_file(ffmpeg) {
        return false;
    }
    let args = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=32x32:r=1:d=0.05",
        "-frames:v",
        "1",
        "-c:v",
        "h264_nvenc",
        "-f",
        "null",
        "-",
    ]
    .iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    run_bounded_command_with_limits(
        ffmpeg,
        &args,
        TOOL_INSPECTION_TIMEOUT,
        MAX_TOOL_INSPECTION_BYTES,
        MAX_TOOL_INSPECTION_BYTES,
    )
    .map(|output| output.status_success)
    .unwrap_or(false)
}

fn version_key(version: Option<&str>) -> (u64, u64, u64, u64) {
    let mut values = version
        .unwrap_or_default()
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok());
    (
        values.next().unwrap_or(0),
        values.next().unwrap_or(0),
        values.next().unwrap_or(0),
        values.next().unwrap_or(0),
    )
}

fn collect_named_binaries(
    root: &Path,
    names: &[&str],
    max_depth: usize,
    max_entries: usize,
    output: &mut Vec<PathBuf>,
) {
    if max_depth == 0 || max_entries == 0 || !root.is_dir() {
        return;
    }
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    while let Some((directory, depth)) = queue.pop_front() {
        if visited >= max_entries || depth >= max_depth {
            break;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten().take(512) {
            visited += 1;
            if visited > max_entries {
                break;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file()
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| names.contains(&name))
            {
                output.push(path);
            } else if kind.is_dir() && depth + 1 < max_depth {
                queue.push_back((path, depth + 1));
            }
        }
    }
}

pub fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MediaProbe {
    pub path: PathBuf,
    pub format_name: Option<String>,
    pub format_long_name: Option<String>,
    pub duration_us: i64,
    pub start_time_us: Option<i64>,
    pub size_bytes: u64,
    pub bit_rate: Option<u64>,
    pub streams: Vec<MediaStreamProbe>,
    pub chapters: Vec<MediaChapterProbe>,
    pub primary_video_stream: Option<u32>,
    pub primary_audio_stream: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MediaStreamProbe {
    pub index: u32,
    pub codec_type: String,
    pub codec_name: Option<String>,
    pub codec_long_name: Option<String>,
    pub time_base: Option<String>,
    pub start_time_us: Option<i64>,
    pub duration_us: Option<i64>,
    pub duration_ts: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pixel_format: Option<String>,
    pub average_frame_rate: Option<String>,
    pub real_frame_rate: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub channel_layout: Option<String>,
    pub language: Option<String>,
    pub rotation_degrees: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MediaChapterProbe {
    pub id: Option<i64>,
    pub start_us: i64,
    pub end_us: i64,
    pub title: Option<String>,
}

/// Validates an untrusted user-selected media file without invoking a media process.
/// Renamed network/playlist manifests fail here; the returned path is canonical and size
/// bounded. Callers that launch FFmpeg should use [`local_media_input_args`] so a manifest
/// that evades or races this content check still cannot select an indirection demuxer or a
/// network protocol.
pub fn validate_local_media_source(path: &Path) -> Result<PathBuf, MediaError> {
    validate_local_media_source_with_size(path).map(|(path, _)| path)
}

/// Produces the complete security-sensitive FFmpeg input argument group, including `-i`.
/// Append these arguments together at the intended input position; do not append a second
/// bare `-i <path>` for an untrusted source.
/// Find the video generator's weights inside one installed model directory.
///
/// Filenames are matched by role rather than listed exactly, because the community publishes these
/// weights under many names and quantizations and soundAr should accept whichever the user
/// installed.
///
/// The denoiser must be a distilled ("turbo") checkpoint. soundAr generates at eight steps with
/// guidance fixed at 1.0, which is what a distilled model wants and what an undistilled one cannot
/// work with: given those settings it emits black frames rather than failing, so picking the wrong
/// checkpoint would spend a minute of compute per shot producing nothing and report success.
/// Everything else prefers the smallest candidate, because on a consumer card the binding
/// constraint is VRAM and a set that loads is worth more than a set that does not.
pub fn resolve_clip_models(directory: &Path) -> Result<ClipModelPaths, MediaError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| {
            MediaError::new(
                "clip_model_missing",
                "The video model directory could not be read",
            )
            .detail(error.to_string())
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();

    let pick = |matches: &dyn Fn(&str) -> bool| -> Option<PathBuf> {
        entries
            .iter()
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .map(str::to_ascii_lowercase)
                    .is_some_and(|name| matches(&name))
            })
            .min_by_key(|path| {
                fs::metadata(path)
                    .map(|meta| meta.len())
                    .unwrap_or(u64::MAX)
            })
            .cloned()
    };

    // The denoiser decides what the clip can be conditioned on; the first-and-last-frame variant
    // is the one soundAr drives, so a reference-only checkpoint is not accepted in its place.
    let denoiser = pick(&|name| {
        name.ends_with(".gguf")
            && name.contains("fl2va")
            && !name.contains("qwen")
            && name.contains("turbo")
    })
    .ok_or_else(|| {
        MediaError::new(
            "clip_model_missing",
            "No distilled first-and-last-frame denoiser was found in the video model directory; \
             soundAr generates at eight steps with guidance 1.0, which needs a turbo checkpoint",
        )
    })?;
    let text_encoder =
        pick(&|name| name.ends_with(".gguf") && name.contains("qwen")).ok_or_else(|| {
            MediaError::new(
                "clip_model_missing",
                "No text encoder was found in the video model directory",
            )
        })?;
    let video_vae = pick(&|name| name.contains("video_vae") && name.ends_with(".safetensors"))
        .ok_or_else(|| {
            MediaError::new(
                "clip_model_missing",
                "No video VAE was found in the video model directory",
            )
        })?;
    // Audio is optional: soundAr scores its own episodes, so a clip's own audio track is not
    // needed and skipping its decode is time not spent.
    let audio_vae = pick(&|name| name.contains("audio_vae") && name.ends_with(".safetensors"));

    Ok(ClipModelPaths {
        denoiser,
        text_encoder,
        video_vae,
        audio_vae,
    })
}

pub fn local_media_input_args(path: &Path) -> Result<Vec<OsString>, MediaError> {
    let source = validate_local_media_source(path)?;
    Ok(vec![
        OsString::from("-protocol_whitelist"),
        OsString::from(LOCAL_MEDIA_PROTOCOL_WHITELIST),
        OsString::from("-format_whitelist"),
        OsString::from(LOCAL_MEDIA_FORMAT_WHITELIST),
        OsString::from("-i"),
        source.into_os_string(),
    ])
}

fn validate_local_media_source_with_size(path: &Path) -> Result<(PathBuf, u64), MediaError> {
    let source = fs::canonicalize(path).map_err(|error| {
        MediaError::new(
            "media_not_found",
            "The selected media file could not be opened",
        )
        .detail(error.to_string())
    })?;
    let metadata = fs::metadata(&source).map_err(|error| {
        MediaError::new(
            "media_not_found",
            "The selected media file could not be inspected",
        )
        .detail(error.to_string())
    })?;
    if !metadata.is_file() {
        return Err(MediaError::new(
            "invalid_media_source",
            "The selected media source is not a regular file",
        ));
    }
    if metadata.len() > MAX_MEDIA_SOURCE_BYTES {
        return Err(MediaError::new(
            "media_source_too_large",
            "The selected media exceeds the local source size limit",
        )
        .detail(format!(
            "{} bytes exceeds the {} byte limit",
            metadata.len(),
            MAX_MEDIA_SOURCE_BYTES
        )));
    }
    reject_local_manifest_content(&source)?;
    Ok((source, metadata.len()))
}

fn reject_local_manifest_content(path: &Path) -> Result<(), MediaError> {
    let mut prefix = Vec::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_MANIFEST_SNIFF_BYTES)
                .read_to_end(&mut prefix)
                .map(|_| ())
        })
        .map_err(|error| {
            MediaError::new(
                "media_not_found",
                "The selected media file could not be inspected safely",
            )
            .detail(error.to_string())
        })?;
    if prefix.is_empty() || prefix.contains(&0) {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&prefix).to_ascii_lowercase();
    let trimmed = text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let lines = trimmed.lines().take(64).collect::<Vec<_>>();
    let looks_like_hls_or_m3u = trimmed.starts_with("#extm3u")
        || (lines
            .iter()
            .any(|line| line.trim_start().starts_with("#extinf"))
            && lines.iter().any(|line| {
                let line = line.trim();
                line.starts_with("http://") || line.starts_with("https://")
            }));
    let looks_like_dash = trimmed.contains("<mpd")
        && (trimmed.contains("urn:mpeg:dash") || trimmed.contains("minimumupdateperiod"));
    let looks_like_playlist = trimmed.starts_with("[playlist]")
        || trimmed.starts_with("ffconcat version")
        || trimmed.contains("<smoothstreamingmedia")
        || (trimmed.contains("<playlist") && trimmed.contains("xspf"))
        || trimmed.starts_with("<asx")
        || looks_like_sdp(&lines);
    if looks_like_hls_or_m3u || looks_like_dash || looks_like_playlist {
        return Err(MediaError::new(
            "unsafe_media_manifest",
            "Playlist and network-bearing manifest files cannot be used as local media",
        ));
    }
    Ok(())
}

fn looks_like_sdp(lines: &[&str]) -> bool {
    let mut version = false;
    let mut origin = false;
    let mut connection_or_media = false;
    for line in lines.iter().map(|line| line.trim()) {
        version |= line == "v=0";
        origin |= line.starts_with("o=");
        connection_or_media |= line.starts_with("c=") || line.starts_with("m=");
    }
    version && origin && connection_or_media
}

pub fn probe_media(path: &Path, ffprobe: &Path) -> Result<MediaProbe, MediaError> {
    let executable = fs::canonicalize(ffprobe).map_err(|error| {
        MediaError::new("ffprobe_unavailable", "FFprobe could not be resolved")
            .detail(error.to_string())
    })?;
    if !is_executable_file(&executable) {
        return Err(MediaError::new(
            "ffprobe_unavailable",
            "The configured FFprobe path is not executable",
        )
        .detail(executable.display().to_string()));
    }
    let (source, source_size) = validate_local_media_source_with_size(path)?;

    let args = [
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-protocol_whitelist"),
        OsString::from(LOCAL_MEDIA_PROTOCOL_WHITELIST),
        OsString::from("-format_whitelist"),
        OsString::from(LOCAL_MEDIA_FORMAT_WHITELIST),
        OsString::from("-print_format"),
        OsString::from("json"),
        OsString::from("-show_format"),
        OsString::from("-show_streams"),
        OsString::from("-show_chapters"),
        OsString::from("-i"),
        source.as_os_str().to_os_string(),
    ];
    let output = run_bounded_command(&executable, &args, FFPROBE_TIMEOUT)?;
    if !output.status_success {
        let mut diagnostic = truncate_diagnostic(&output.stderr, 2_000);
        if output.stderr_truncated {
            diagnostic.push_str("\n[diagnostics truncated at the safety limit]");
        }
        return Err(
            MediaError::new("ffprobe_failed", "FFprobe rejected the selected media")
                .detail(diagnostic),
        );
    }
    if output.stdout_truncated || output.stdout.len() > MAX_PROBE_JSON_BYTES {
        return Err(MediaError::new(
            "ffprobe_output_too_large",
            "FFprobe returned an unexpectedly large response",
        ));
    }
    let raw: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        MediaError::new("ffprobe_invalid_json", "FFprobe returned invalid metadata")
            .detail(error.to_string())
    })?;
    validate_probed_local_format(&raw)?;
    parse_probe_value(&source, source_size, &raw)
}

fn validate_probed_local_format(raw: &Value) -> Result<(), MediaError> {
    let format_name = raw
        .get("format")
        .and_then(|format| format.get("format_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let unsafe_format = format_name.split(',').any(|name| {
        matches!(
            name.trim(),
            "concat"
                | "dash"
                | "ffmetadata"
                | "hls"
                | "image2"
                | "imf"
                | "live_flv"
                | "m3u"
                | "rtsp"
                | "sdp"
                | "smil"
                | "xspf"
        )
    });
    if unsafe_format {
        return Err(MediaError::new(
            "unsafe_media_manifest",
            "The selected file is an indirection manifest, not self-contained media",
        )
        .detail(format_name.to_string()));
    }
    Ok(())
}

#[derive(Debug)]
struct BoundedCommandOutput {
    status_success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

#[derive(Debug)]
struct BoundedPipeCapture {
    bytes: Vec<u8>,
    truncated: bool,
}

fn run_bounded_command(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<BoundedCommandOutput, MediaError> {
    run_bounded_command_with_limits(
        program,
        args,
        timeout,
        MAX_PROBE_JSON_BYTES,
        MAX_MEDIA_PROCESS_STDERR_BYTES,
    )
}

fn run_bounded_command_with_limits(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<BoundedCommandOutput, MediaError> {
    use std::os::unix::process::CommandExt;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            MediaError::new(
                "media_process_failed",
                "The media helper could not be started",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
    let process_group = i32::try_from(child.id()).ok();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    if let Err(error) = set_pipe_nonblocking(&stdout).and_then(|_| set_pipe_nonblocking(&stderr)) {
        terminate_owned_process_group(&mut child);
        return Err(MediaError::new(
            "media_process_failed",
            "The media helper output could not be secured",
        )
        .detail(error.to_string())
        .retryable(true));
    }
    let stop_readers = Arc::new(AtomicBool::new(false));
    let stdout_reader = match spawn_bounded_pipe_reader(
        "soundar-media-stdout",
        stdout,
        stdout_limit,
        Arc::clone(&stop_readers),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_owned_process_group(&mut child);
            return Err(MediaError::new(
                "media_process_failed",
                "The media helper output reader could not be started",
            )
            .detail(error.to_string())
            .retryable(true));
        }
    };
    let stderr_reader = match spawn_bounded_pipe_reader(
        "soundar-media-stderr",
        stderr,
        stderr_limit,
        Arc::clone(&stop_readers),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_owned_process_group(&mut child);
            stop_readers.store(true, Ordering::Release);
            let _ = stdout_reader.join();
            return Err(MediaError::new(
                "media_process_failed",
                "The media helper diagnostics reader could not be started",
            )
            .detail(error.to_string())
            .retryable(true));
        }
    };
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                terminate_owned_process_group(&mut child);
                let _ =
                    finish_pipe_readers(stdout_reader, stderr_reader, stop_readers, process_group);
                return Err(MediaError::new(
                    "media_process_timeout",
                    "The media inspection process timed out",
                )
                .retryable(true));
            }
            Err(error) => {
                terminate_owned_process_group(&mut child);
                let _ =
                    finish_pipe_readers(stdout_reader, stderr_reader, stop_readers, process_group);
                return Err(MediaError::new(
                    "media_process_failed",
                    "The media inspection process could not be monitored",
                )
                .detail(error.to_string())
                .retryable(true));
            }
        }
    };
    let (stdout, stderr, fully_drained) =
        finish_pipe_readers(stdout_reader, stderr_reader, stop_readers, process_group)?;
    if !fully_drained {
        return Err(MediaError::new(
            "media_process_failed",
            "The media helper left its output pipes open after exiting",
        )
        .retryable(true));
    }
    Ok(BoundedCommandOutput {
        status_success: status.success(),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

fn set_pipe_nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    let descriptor = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn spawn_bounded_pipe_reader<R>(
    name: &str,
    reader: R,
    limit: usize,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<std::io::Result<BoundedPipeCapture>>>
where
    R: std::io::Read + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || drain_bounded_pipe(reader, limit, &stop))
}

fn drain_bounded_pipe(
    mut reader: impl std::io::Read,
    limit: usize,
    stop: &AtomicBool,
) -> std::io::Result<BoundedPipeCapture> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                let retained = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..retained]);
                truncated |= retained < count;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(PROCESS_PIPE_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(BoundedPipeCapture { bytes, truncated })
}

fn finish_pipe_readers(
    stdout_reader: JoinHandle<std::io::Result<BoundedPipeCapture>>,
    stderr_reader: JoinHandle<std::io::Result<BoundedPipeCapture>>,
    stop: Arc<AtomicBool>,
    process_group: Option<i32>,
) -> Result<(BoundedPipeCapture, BoundedPipeCapture, bool), MediaError> {
    let drain_deadline = Instant::now()
        .checked_add(PROCESS_PIPE_DRAIN_TIMEOUT)
        .unwrap_or_else(Instant::now);
    while !(stdout_reader.is_finished() && stderr_reader.is_finished())
        && Instant::now() < drain_deadline
    {
        thread::sleep(PROCESS_PIPE_POLL_INTERVAL);
    }
    let fully_drained = stdout_reader.is_finished() && stderr_reader.is_finished();
    if !fully_drained {
        terminate_process_group(process_group);
    }
    // Nonblocking readers make this a bounded join even if a descendant escaped the
    // owned process group while retaining one of the inherited pipe descriptors.
    stop.store(true, Ordering::Release);
    let stdout = join_pipe_reader(stdout_reader, "output")?;
    let stderr = join_pipe_reader(stderr_reader, "diagnostics")?;
    Ok((stdout, stderr, fully_drained))
}

fn join_pipe_reader(
    reader: JoinHandle<std::io::Result<BoundedPipeCapture>>,
    label: &str,
) -> Result<BoundedPipeCapture, MediaError> {
    reader
        .join()
        .map_err(|_| {
            MediaError::new(
                "media_process_failed",
                format!("The media helper {label} reader failed"),
            )
        })?
        .map_err(|error| {
            MediaError::new(
                "media_process_failed",
                format!("The media helper {label} could not be read"),
            )
            .detail(error.to_string())
        })
}

fn terminate_owned_process_group(child: &mut Child) {
    terminate_process_group(i32::try_from(child.id()).ok());
    let _ = child.kill();
    let _ = child.wait();
}

fn terminate_process_group(process_group: Option<i32>) {
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

fn parse_probe_value(
    path: &Path,
    filesystem_size: u64,
    raw: &Value,
) -> Result<MediaProbe, MediaError> {
    let format = raw.get("format").and_then(Value::as_object);
    let streams_value = raw
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                "invalid_media",
                "The media contains no readable stream metadata",
            )
        })?;
    let mut streams = Vec::with_capacity(streams_value.len());
    for stream in streams_value {
        let object = stream.as_object().ok_or_else(|| {
            MediaError::new("invalid_media", "FFprobe returned an invalid stream record")
        })?;
        let index = value_u64(object.get("index"))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| MediaError::new("invalid_media", "A media stream has no index"))?;
        let codec_type = value_string(object.get("codec_type")).unwrap_or_else(|| "unknown".into());
        let tags = object.get("tags").and_then(Value::as_object);
        let side_data = object.get("side_data_list").and_then(Value::as_array);
        streams.push(MediaStreamProbe {
            index,
            codec_type,
            codec_name: value_string(object.get("codec_name")),
            codec_long_name: value_string(object.get("codec_long_name")),
            time_base: value_string(object.get("time_base")),
            start_time_us: value_string(object.get("start_time"))
                .as_deref()
                .and_then(decimal_seconds_to_us),
            duration_us: value_string(object.get("duration"))
                .as_deref()
                .and_then(decimal_seconds_to_us),
            duration_ts: value_i64(object.get("duration_ts")),
            width: value_u64(object.get("width")).and_then(|value| u32::try_from(value).ok()),
            height: value_u64(object.get("height")).and_then(|value| u32::try_from(value).ok()),
            pixel_format: value_string(object.get("pix_fmt")),
            average_frame_rate: value_string(object.get("avg_frame_rate"))
                .filter(|value| value != "0/0"),
            real_frame_rate: value_string(object.get("r_frame_rate"))
                .filter(|value| value != "0/0"),
            sample_rate: value_string(object.get("sample_rate"))
                .and_then(|value| value.parse().ok()),
            channels: value_u64(object.get("channels")).and_then(|value| u16::try_from(value).ok()),
            channel_layout: value_string(object.get("channel_layout")),
            language: tags.and_then(|tags| value_string(tags.get("language"))),
            rotation_degrees: parse_rotation(side_data, tags),
        });
    }
    if streams.is_empty()
        || !streams
            .iter()
            .any(|stream| matches!(stream.codec_type.as_str(), "video" | "audio"))
    {
        return Err(MediaError::new(
            "invalid_media",
            "The selected file contains no playable audio or video stream",
        ));
    }

    let duration_us = format
        .and_then(|value| value_string(value.get("duration")))
        .as_deref()
        .and_then(decimal_seconds_to_us)
        .or_else(|| streams.iter().filter_map(|stream| stream.duration_us).max())
        .ok_or_else(|| {
            MediaError::new(
                "invalid_media_duration",
                "The media duration could not be determined",
            )
        })?;
    if duration_us <= 0 {
        return Err(MediaError::new(
            "invalid_media_duration",
            "The selected media has no positive duration",
        ));
    }
    let chapters = raw
        .get("chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_chapter)
        .collect::<Vec<_>>();
    let primary_video_stream = streams
        .iter()
        .find(|stream| stream.codec_type == "video")
        .map(|stream| stream.index);
    let primary_audio_stream = streams
        .iter()
        .find(|stream| stream.codec_type == "audio")
        .map(|stream| stream.index);
    Ok(MediaProbe {
        path: path.to_path_buf(),
        format_name: format.and_then(|value| value_string(value.get("format_name"))),
        format_long_name: format.and_then(|value| value_string(value.get("format_long_name"))),
        duration_us,
        start_time_us: format
            .and_then(|value| value_string(value.get("start_time")))
            .as_deref()
            .and_then(decimal_seconds_to_us),
        size_bytes: format
            .and_then(|value| value_string(value.get("size")))
            .and_then(|value| value.parse().ok())
            .unwrap_or(filesystem_size),
        bit_rate: format
            .and_then(|value| value_string(value.get("bit_rate")))
            .and_then(|value| value.parse().ok()),
        streams,
        chapters,
        primary_video_stream,
        primary_audio_stream,
    })
}

fn parse_rotation(
    side_data: Option<&Vec<Value>>,
    tags: Option<&serde_json::Map<String, Value>>,
) -> Option<i32> {
    side_data
        .into_iter()
        .flatten()
        .find_map(|entry| {
            entry
                .get("rotation")
                .and_then(|value| value_i64(Some(value)))
        })
        .and_then(|value| i32::try_from(value).ok())
        .or_else(|| {
            tags.and_then(|tags| value_string(tags.get("rotate")))
                .and_then(|value| value.parse().ok())
        })
}

fn parse_chapter(value: &Value) -> Option<MediaChapterProbe> {
    let object = value.as_object()?;
    let start_us = value_string(object.get("start_time"))
        .as_deref()
        .and_then(decimal_seconds_to_us)?;
    let end_us = value_string(object.get("end_time"))
        .as_deref()
        .and_then(decimal_seconds_to_us)?;
    if start_us < 0 || end_us <= start_us {
        return None;
    }
    let tags = object.get("tags").and_then(Value::as_object);
    Some(MediaChapterProbe {
        id: value_i64(object.get("id")),
        start_us,
        end_us,
        title: tags.and_then(|tags| value_string(tags.get("title"))),
    })
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn value_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn decimal_seconds_to_us(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n/a") {
        return None;
    }
    let (negative, unsigned) = if let Some(value) = value.strip_prefix('-') {
        (true, value)
    } else if let Some(value) = value.strip_prefix('+') {
        (false, value)
    } else {
        (false, value)
    };
    let mut parts = unsigned.split('.');
    let whole = parts.next()?.parse::<i128>().ok()?;
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some() || !fraction.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let mut micros_text = fraction.chars().take(6).collect::<String>();
    while micros_text.len() < 6 {
        micros_text.push('0');
    }
    let mut micros = if micros_text.is_empty() {
        0i128
    } else {
        micros_text.parse().ok()?
    };
    if fraction
        .as_bytes()
        .get(6)
        .is_some_and(|digit| *digit >= b'5')
    {
        micros += 1;
    }
    let mut total = whole.checked_mul(1_000_000)?.checked_add(micros)?;
    if negative {
        total = -total;
    }
    i64::try_from(total).ok()
}

fn truncate_diagnostic(bytes: &[u8], max_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportProvider {
    YouTube,
    Generic,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ValidatedImportUrl {
    pub original: String,
    pub canonical: String,
    pub host: String,
    pub provider: ImportProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub is_playlist: bool,
    pub rights_confirmation_required: bool,
}

/// Resolves the canonical import authority for early intake feedback and rejects the
/// complete answer set if any address is private, local, documentation-only, or otherwise
/// non-public. This check intentionally stays separate from [`validate_import_url`] so syntax
/// validation remains deterministic and offline.
///
/// This preflight does not pin a later downloader resolution and cannot inspect redirect
/// authorities. Production yt-dlp execution must therefore be held behind
/// [`PublicHttpsProxy`], which repeats this policy for every CONNECT request and connects to
/// the validated address directly.
pub fn preflight_import_url_destination(
    validated: &ValidatedImportUrl,
) -> Result<Vec<IpAddr>, MediaError> {
    let original = validate_import_url(&validated.original)?;
    if original != *validated {
        return Err(MediaError::new(
            "invalid_url_preflight",
            "The import URL contract changed after validation",
        ));
    }
    let canonical = validate_import_url(&validated.canonical)?;
    resolve_public_import_host(&canonical.host, None)
}

/// Authenticated, loopback-only HTTPS CONNECT proxy for untrusted downloader traffic.
///
/// Each CONNECT authority is independently resolved, the entire DNS answer set must be
/// public, and the upstream socket is opened against one of those validated IP addresses.
/// This pins the checked resolution and applies the same rule to redirects. Non-CONNECT
/// requests fail closed, so an HTTPS-to-HTTP downgrade cannot bypass the address policy.
/// Keep this value alive for the entire child-process lifetime and pass [`Self::url`] to
/// yt-dlp's `--proxy` option.
pub struct PublicHttpsProxy {
    proxy_url: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl PublicHttpsProxy {
    pub fn start() -> Result<Self, MediaError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|error| {
            MediaError::new(
                "import_proxy_unavailable",
                "The protected link-import proxy could not be started",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            MediaError::new(
                "import_proxy_unavailable",
                "The protected link-import proxy could not be configured",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
        let address = listener.local_addr().map_err(|error| {
            MediaError::new(
                "import_proxy_unavailable",
                "The protected link-import proxy address could not be read",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let credentials = format!("soundar:{token}");
        let authorization =
            Arc::<str>::from(format!("Basic {}", encode_base64(credentials.as_bytes())));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let accept_thread = thread::Builder::new()
            .name("soundar-import-proxy".to_string())
            .spawn(move || run_public_https_proxy(listener, authorization, worker_stop))
            .map_err(|error| {
                MediaError::new(
                    "import_proxy_unavailable",
                    "The protected link-import proxy worker could not be started",
                )
                .detail(error.to_string())
                .retryable(true)
            })?;
        Ok(Self {
            proxy_url: format!("http://soundar:{token}@{address}"),
            address,
            stop,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn url(&self) -> &str {
        &self.proxy_url
    }
}

impl fmt::Debug for PublicHttpsProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicHttpsProxy")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Drop for PublicHttpsProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(worker) = self.accept_thread.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy)]
enum ProxyFailure {
    BadRequest,
    AuthenticationRequired,
    MethodNotAllowed,
    Forbidden,
    BadGateway,
    ServiceUnavailable,
}

impl ProxyFailure {
    fn response(self) -> &'static [u8] {
        match self {
            Self::BadRequest => {
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::AuthenticationRequired => b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"soundAr link import\"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            Self::MethodNotAllowed => b"HTTP/1.1 405 Method Not Allowed\r\nAllow: CONNECT\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
            Self::Forbidden => {
                b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::BadGateway => {
                b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::ServiceUnavailable => b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        }
    }
}

struct ProxyConnectRequest {
    host: String,
    initial_payload: Vec<u8>,
}

fn run_public_https_proxy(listener: TcpListener, authorization: Arc<str>, stop: Arc<AtomicBool>) {
    let mut handlers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let mut index = 0;
        while index < handlers.len() {
            if handlers[index].is_finished() {
                let handler = handlers.swap_remove(index);
                let _ = handler.join();
            } else {
                index += 1;
            }
        }
        match listener.accept() {
            Ok((mut client, _)) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if handlers.len() >= MAX_PROXY_CONNECTIONS {
                    write_proxy_failure(&mut client, ProxyFailure::ServiceUnavailable);
                    continue;
                }
                let handler_authorization = Arc::clone(&authorization);
                let handler_stop = Arc::clone(&stop);
                match thread::Builder::new()
                    .name("soundar-import-tunnel".to_string())
                    .spawn(move || {
                        handle_public_proxy_connection(client, &handler_authorization, handler_stop)
                    }) {
                    Ok(handler) => handlers.push(handler),
                    Err(_) => {
                        // `client` is dropped with the failed spawn closure. The listener
                        // remains healthy and later requests can retry.
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(PROXY_ACCEPT_POLL_INTERVAL);
            }
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::Release);
    for handler in handlers {
        let _ = handler.join();
    }
}

fn handle_public_proxy_connection(
    mut client: TcpStream,
    authorization: &str,
    stop: Arc<AtomicBool>,
) {
    let _ = client.set_nodelay(true);
    let request = match read_proxy_connect_request(&mut client, authorization, &stop) {
        Ok(request) => request,
        Err(failure) => {
            write_proxy_failure(&mut client, failure);
            return;
        }
    };
    let mut upstream = match connect_public_import_host(&request.host, &stop) {
        Ok(upstream) => upstream,
        Err(error) => {
            let failure = if matches!(
                error.code.as_str(),
                "unsafe_url_destination" | "unsafe_url_host"
            ) {
                ProxyFailure::Forbidden
            } else {
                ProxyFailure::BadGateway
            };
            write_proxy_failure(&mut client, failure);
            return;
        }
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    if !request.initial_payload.is_empty() && upstream.write_all(&request.initial_payload).is_err()
    {
        return;
    }
    relay_proxy_tunnel(client, upstream, stop);
}

fn read_proxy_connect_request(
    client: &mut TcpStream,
    expected_authorization: &str,
    stop: &AtomicBool,
) -> Result<ProxyConnectRequest, ProxyFailure> {
    client
        .set_read_timeout(Some(PROXY_IO_POLL_TIMEOUT))
        .map_err(|_| ProxyFailure::BadRequest)?;
    client
        .set_write_timeout(Some(PROXY_IO_POLL_TIMEOUT))
        .map_err(|_| ProxyFailure::BadRequest)?;
    let deadline = Instant::now()
        .checked_add(PROXY_HEADER_TIMEOUT)
        .unwrap_or_else(Instant::now);
    let mut request = Vec::with_capacity(2 * 1024);
    let mut chunk = [0u8; 2 * 1024];
    let header_end = loop {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(ProxyFailure::BadRequest);
        }
        match client.read(&mut chunk) {
            Ok(0) => return Err(ProxyFailure::BadRequest),
            Ok(count) => {
                request.extend_from_slice(&chunk[..count]);
                if let Some(end) = find_header_end(&request) {
                    if end > MAX_PROXY_CONNECT_HEADER_BYTES {
                        return Err(ProxyFailure::BadRequest);
                    }
                    break end;
                }
                if request.len() > MAX_PROXY_CONNECT_HEADER_BYTES {
                    return Err(ProxyFailure::BadRequest);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ProxyFailure::BadRequest),
        }
    };
    let header =
        std::str::from_utf8(&request[..header_end]).map_err(|_| ProxyFailure::BadRequest)?;
    if !header.is_ascii() {
        return Err(ProxyFailure::BadRequest);
    }
    let mut lines = header.split("\r\n");
    let request_line = lines.next().ok_or(ProxyFailure::BadRequest)?;
    if request_line.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ProxyFailure::BadRequest);
    }
    let parts = request_line.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
        return Err(ProxyFailure::BadRequest);
    }
    let mut supplied_authorization: Option<&str> = None;
    for line in lines.filter(|line| !line.is_empty()) {
        if line.starts_with([' ', '\t'])
            || line
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(ProxyFailure::BadRequest);
        }
        let (name, value) = line.split_once(':').ok_or(ProxyFailure::BadRequest)?;
        if name.is_empty() || !name.bytes().all(is_http_token_byte) {
            return Err(ProxyFailure::BadRequest);
        }
        if name.eq_ignore_ascii_case("proxy-authorization") {
            if supplied_authorization.is_some() {
                return Err(ProxyFailure::AuthenticationRequired);
            }
            supplied_authorization = Some(value.trim());
        }
    }
    if !supplied_authorization.is_some_and(|value| {
        constant_time_equal(value.as_bytes(), expected_authorization.as_bytes())
    }) {
        return Err(ProxyFailure::AuthenticationRequired);
    }
    if parts[0] != "CONNECT" {
        return Err(ProxyFailure::MethodNotAllowed);
    }
    let host = parse_connect_authority(parts[1])?;
    Ok(ProxyConnectRequest {
        host,
        initial_payload: request[header_end..].to_vec(),
    })
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_connect_authority(authority: &str) -> Result<String, ProxyFailure> {
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || authority.chars().any(char::is_whitespace)
    {
        return Err(ProxyFailure::BadRequest);
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']').ok_or(ProxyFailure::BadRequest)?;
        if suffix != ":443" || host.contains('%') {
            return Err(ProxyFailure::Forbidden);
        }
        let address = host
            .parse::<Ipv6Addr>()
            .map_err(|_| ProxyFailure::BadRequest)?;
        if !is_public_network_address(IpAddr::V6(address)) {
            return Err(ProxyFailure::Forbidden);
        }
        return Ok(address.to_string());
    }
    let (host, port) = authority.rsplit_once(':').ok_or(ProxyFailure::BadRequest)?;
    if port != "443" || host.contains(':') {
        return Err(ProxyFailure::Forbidden);
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    validate_public_hostname(&host).map_err(|_| ProxyFailure::Forbidden)?;
    Ok(host)
}

fn write_proxy_failure(client: &mut TcpStream, failure: ProxyFailure) {
    let _ = client.set_write_timeout(Some(PROXY_IO_POLL_TIMEOUT));
    let _ = client.write_all(failure.response());
}

fn relay_proxy_tunnel(client: TcpStream, upstream: TcpStream, stop: Arc<AtomicBool>) {
    let _ = client.set_read_timeout(Some(PROXY_IO_POLL_TIMEOUT));
    let _ = client.set_write_timeout(Some(PROXY_IO_POLL_TIMEOUT));
    let _ = upstream.set_read_timeout(Some(PROXY_IO_POLL_TIMEOUT));
    let _ = upstream.set_write_timeout(Some(PROXY_IO_POLL_TIMEOUT));
    let client_reader = match client.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let upstream_writer = match upstream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let connection_stop = Arc::new(AtomicBool::new(false));
    let upload_stop = Arc::clone(&connection_stop);
    let upload_global_stop = Arc::clone(&stop);
    let upload = match thread::Builder::new()
        .name("soundar-import-upload".to_string())
        .spawn(move || {
            relay_proxy_direction(
                client_reader,
                upstream_writer,
                &upload_global_stop,
                &upload_stop,
            )
        }) {
        Ok(worker) => worker,
        Err(_) => return,
    };
    let upstream_reader = match upstream.try_clone() {
        Ok(stream) => stream,
        Err(_) => {
            connection_stop.store(true, Ordering::Release);
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = upstream.shutdown(std::net::Shutdown::Both);
            let _ = upload.join();
            return;
        }
    };
    let client_writer = match client.try_clone() {
        Ok(stream) => stream,
        Err(_) => {
            connection_stop.store(true, Ordering::Release);
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = upstream.shutdown(std::net::Shutdown::Both);
            let _ = upload.join();
            return;
        }
    };
    relay_proxy_direction(upstream_reader, client_writer, &stop, &connection_stop);
    connection_stop.store(true, Ordering::Release);
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = upstream.shutdown(std::net::Shutdown::Both);
    let _ = upload.join();
}

fn relay_proxy_direction(
    mut source: TcpStream,
    mut destination: TcpStream,
    global_stop: &AtomicBool,
    connection_stop: &AtomicBool,
) {
    let mut buffer = [0u8; PROXY_RELAY_BUFFER_BYTES];
    while !global_stop.load(Ordering::Acquire) && !connection_stop.load(Ordering::Acquire) {
        let count = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut written = 0;
        while written < count
            && !global_stop.load(Ordering::Acquire)
            && !connection_stop.load(Ordering::Acquire)
        {
            match destination.write(&buffer[written..count]) {
                Ok(0) => {
                    connection_stop.store(true, Ordering::Release);
                    let _ = source.shutdown(std::net::Shutdown::Both);
                    let _ = destination.shutdown(std::net::Shutdown::Both);
                    return;
                }
                Ok(count) => written += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    connection_stop.store(true, Ordering::Release);
                    let _ = source.shutdown(std::net::Shutdown::Both);
                    let _ = destination.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        }
    }
    connection_stop.store(true, Ordering::Release);
    let _ = source.shutdown(std::net::Shutdown::Both);
    let _ = destination.shutdown(std::net::Shutdown::Both);
}

fn connect_public_import_host(host: &str, stop: &AtomicBool) -> Result<TcpStream, MediaError> {
    let addresses = resolve_public_import_host(host, Some(stop))?;
    let deadline = Instant::now()
        .checked_add(PROXY_CONNECT_TIMEOUT)
        .unwrap_or_else(Instant::now);
    let mut last_error = None;
    for address in addresses {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let attempt_timeout = remaining.min(PROXY_CONNECT_ATTEMPT_TIMEOUT);
        match TcpStream::connect_timeout(&SocketAddr::new(address, 443), attempt_timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(MediaError::new(
        "url_destination_unreachable",
        "The public media destination could not be reached",
    )
    .detail(last_error.unwrap_or_else(|| "connection cancelled or timed out".to_string()))
    .retryable(true))
}

fn resolve_public_import_host(
    host: &str,
    stop: Option<&AtomicBool>,
) -> Result<Vec<IpAddr>, MediaError> {
    if let Ok(address) = host.parse::<IpAddr>() {
        return validate_public_destination_addresses(host, vec![address]);
    }
    validate_public_hostname(host)?;
    let owned_host = host.to_string();
    let resolver_host = owned_host.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    let resolver = thread::Builder::new()
        .name("soundar-import-dns".to_string())
        .spawn(move || {
            let result = (resolver_host.as_str(), 443)
                .to_socket_addrs()
                .map_err(|error| error.to_string())
                .and_then(|resolved| {
                    let mut addresses = Vec::new();
                    for socket in resolved {
                        let address = socket.ip();
                        if addresses.contains(&address) {
                            continue;
                        }
                        if addresses.len() >= MAX_IMPORT_DNS_ADDRESSES {
                            return Err(format!(
                                "DNS returned more than {MAX_IMPORT_DNS_ADDRESSES} addresses"
                            ));
                        }
                        addresses.push(address);
                    }
                    Ok(addresses)
                });
            let _ = sender.send(result);
        })
        .map_err(|error| {
            MediaError::new(
                "url_dns_failed",
                "The media host resolver could not be started",
            )
            .detail(error.to_string())
            .retryable(true)
        })?;
    let deadline = Instant::now()
        .checked_add(IMPORT_DNS_TIMEOUT)
        .unwrap_or_else(Instant::now);
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err(MediaError::new(
                "url_dns_cancelled",
                "The media host lookup was cancelled",
            )
            .retryable(true));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(
                MediaError::new("url_dns_timeout", "The media host lookup timed out")
                    .retryable(true),
            );
        }
        match receiver.recv_timeout(
            deadline
                .saturating_duration_since(now)
                .min(IMPORT_DNS_POLL_INTERVAL),
        ) {
            Ok(Ok(addresses)) => {
                let _ = resolver.join();
                return validate_public_destination_addresses(&owned_host, addresses);
            }
            Ok(Err(detail)) => {
                let _ = resolver.join();
                return Err(MediaError::new(
                    "url_dns_failed",
                    "The media host could not be resolved",
                )
                .detail(detail)
                .retryable(true));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = resolver.join();
                return Err(MediaError::new(
                    "url_dns_failed",
                    "The media host resolver stopped unexpectedly",
                )
                .retryable(true));
            }
        }
    }
}

fn validate_public_destination_addresses(
    host: &str,
    addresses: Vec<IpAddr>,
) -> Result<Vec<IpAddr>, MediaError> {
    if addresses.is_empty() {
        return Err(MediaError::new(
            "url_dns_failed",
            "The media host returned no usable network address",
        )
        .detail(host.to_string())
        .retryable(true));
    }
    if let Some(address) = addresses
        .iter()
        .copied()
        .find(|address| !is_public_network_address(*address))
    {
        return Err(MediaError::new(
            "unsafe_url_destination",
            "The media host resolves to a private or reserved destination",
        )
        .detail(format!("{host} resolved to {address}")));
    }
    Ok(addresses)
}

fn is_public_network_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !matches!(
        (first, second, third),
        (0, _, _)
            | (10, _, _)
            | (100, 64..=127, _)
            | (127, _, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0)
            | (192, 0, 2)
            | (192, 88, 99)
            | (192, 168, _)
            | (198, 18..=19, _)
            | (198, 51, 100)
            | (203, 0, 113)
            | (224..=255, _, _)
    )
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let value = u128::from(address);
    // Current globally routable unicast space is 2000::/3. Conservatively reject
    // non-global, transition, benchmarking, documentation and ORCHID allocations inside it.
    ipv6_has_prefix(value, 0x2000_u128 << 112, 3)
        && !ipv6_has_prefix(value, 0x2001_u128 << 112, 23)
        && !ipv6_has_prefix(value, 0x2001_0002_u128 << 96, 48)
        && !ipv6_has_prefix(value, 0x2001_0010_u128 << 96, 28)
        && !ipv6_has_prefix(value, 0x2001_0020_u128 << 96, 28)
        && !ipv6_has_prefix(value, 0x2001_0db8_u128 << 96, 32)
        && !ipv6_has_prefix(value, 0x2002_u128 << 112, 16)
        && !ipv6_has_prefix(value, 0x3fff_u128 << 112, 20)
}

fn ipv6_has_prefix(address: u128, network: u128, prefix_length: u32) -> bool {
    let mask = u128::MAX << (128 - prefix_length);
    address & mask == network & mask
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

pub fn validate_import_url(raw: &str) -> Result<ValidatedImportUrl, MediaError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(MediaError::new("url_required", "Enter one media URL"));
    }
    if value.len() > MAX_IMPORT_URL_BYTES {
        return Err(MediaError::new("url_too_long", "The media URL is too long"));
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(MediaError::new(
            "invalid_url",
            "Enter one URL without spaces or control characters",
        ));
    }
    let scheme_end = value.find("://").ok_or_else(|| {
        MediaError::new("invalid_url", "The media URL must include an HTTPS scheme")
    })?;
    if !value[..scheme_end].eq_ignore_ascii_case("https") {
        return Err(MediaError::new(
            "insecure_url",
            "Only HTTPS media URLs are supported",
        ));
    }
    let after_scheme = &value[scheme_end + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return Err(MediaError::new(
            "invalid_url",
            "Media URLs may not contain credentials",
        ));
    }
    if authority.starts_with('[') || authority.matches(':').count() > 1 {
        return Err(MediaError::new(
            "unsupported_url_host",
            "IPv6 link sources are not supported",
        ));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    if port.is_some_and(|port| port != "443") {
        return Err(MediaError::new(
            "unsupported_url_port",
            "Media URLs may only use the standard HTTPS port",
        ));
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    validate_public_hostname(&host)?;

    let remainder = &after_scheme[authority_end..];
    let without_fragment = remainder.split('#').next().unwrap_or_default();
    let (path, query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, ""), |(path, query)| (path, query));
    let query_pairs = parse_query_pairs(query)?;
    let playlist_key = query_pairs.iter().any(|(key, _)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "list" | "playlist" | "album"
        )
    });
    let playlist_path = path
        .split('/')
        .any(|part| matches!(part.to_ascii_lowercase().as_str(), "playlist" | "playlists"));
    if playlist_key || playlist_path {
        return Err(MediaError::new(
            "playlist_not_allowed",
            "Import one authorized video at a time; playlists are not supported",
        ));
    }

    let youtube_host = matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    );
    if youtube_host {
        let source_id = youtube_video_id(&host, path, &query_pairs)?;
        return Ok(ValidatedImportUrl {
            original: value.to_string(),
            canonical: format!("https://www.youtube.com/watch?v={source_id}"),
            host,
            provider: ImportProvider::YouTube,
            source_id: Some(source_id),
            is_playlist: false,
            rights_confirmation_required: true,
        });
    }
    let canonical_authority = if port.is_some() {
        format!("{host}:443")
    } else {
        host.clone()
    };
    Ok(ValidatedImportUrl {
        original: value.to_string(),
        canonical: format!("https://{canonical_authority}{without_fragment}"),
        host,
        provider: ImportProvider::Generic,
        source_id: None,
        is_playlist: false,
        rights_confirmation_required: true,
    })
}

fn validate_public_hostname(host: &str) -> Result<(), MediaError> {
    if host.is_empty()
        || !host.is_ascii()
        || host.len() > 253
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err(MediaError::new(
            "unsafe_url_host",
            "The media URL must use a public host",
        ));
    }
    if let Ok(address) = host.parse::<std::net::Ipv4Addr>() {
        if !is_public_network_address(IpAddr::V4(address)) {
            return Err(MediaError::new(
                "unsafe_url_host",
                "Private and reserved media hosts are not supported",
            ));
        }
        return Ok(());
    }
    if !host.contains('.')
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
    {
        return Err(MediaError::new(
            "invalid_url_host",
            "The media URL host is invalid",
        ));
    }
    Ok(())
}

fn parse_query_pairs(query: &str) -> Result<Vec<(String, String)>, MediaError> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((percent_decode_ascii(key)?, percent_decode_ascii(value)?))
        })
        .collect()
}

fn percent_decode_ascii(value: &str) -> Result<String, MediaError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(MediaError::new(
                    "invalid_url_encoding",
                    "The media URL contains invalid percent encoding",
                ));
            }
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            let decoded = high
                .zip(low)
                .map(|(high, low)| high * 16 + low)
                .ok_or_else(|| {
                    MediaError::new(
                        "invalid_url_encoding",
                        "The media URL contains invalid percent encoding",
                    )
                })?;
            if decoded == 0 || decoded.is_ascii_control() {
                return Err(MediaError::new(
                    "invalid_url_encoding",
                    "The media URL contains an unsafe encoded character",
                ));
            }
            output.push(decoded);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| {
        MediaError::new(
            "invalid_url_encoding",
            "The media URL query is not valid UTF-8",
        )
    })
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn youtube_video_id(
    host: &str,
    path: &str,
    query: &[(String, String)],
) -> Result<String, MediaError> {
    let segments = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let candidate = if host == "youtu.be" {
        (segments.len() == 1).then_some(segments[0])
    } else if segments.as_slice() == ["watch"] || segments.is_empty() {
        query
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("v"))
            .map(|(_, value)| value.as_str())
    } else if segments.len() == 2
        && matches!(
            segments[0].to_ascii_lowercase().as_str(),
            "shorts" | "live" | "embed"
        )
    {
        Some(segments[1])
    } else {
        None
    };
    let candidate = candidate.ok_or_else(|| {
        MediaError::new(
            "unsupported_youtube_url",
            "Enter a direct YouTube video, Short, or livestream URL",
        )
    })?;
    if !(6..=32).contains(&candidate.len())
        || !candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(MediaError::new(
            "invalid_youtube_id",
            "The YouTube video identifier is invalid",
        ));
    }
    Ok(candidate.to_string())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptionCueInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptionValidation {
    pub cues: Vec<CaptionCueInput>,
    pub duration_us: i64,
    pub spoken_span_us: i64,
    pub preserved_gap_us: i64,
    pub overlap_count: u32,
    pub sha256: String,
}

pub fn validate_caption_cues(
    cues: &[CaptionCueInput],
    duration_us: i64,
) -> Result<CaptionValidation, MediaError> {
    if duration_us <= 0 {
        return Err(MediaError::new(
            "invalid_caption_duration",
            "Caption validation requires a positive source duration",
        ));
    }
    if cues.is_empty() {
        return Err(MediaError::new(
            "empty_captions",
            "The caption source contains no speech cues",
        ));
    }
    if cues.len() > MAX_CAPTION_CUES {
        return Err(MediaError::new(
            "captions_too_large",
            "The caption source contains too many cues",
        ));
    }

    let mut previous_start = -1i64;
    let mut previous_end = -1i64;
    let mut union_start = 0i64;
    let mut union_end = 0i64;
    let mut spoken_span_us = 0i64;
    let mut overlap_count = 0u32;
    let mut hasher = Sha256::new();
    for (index, cue) in cues.iter().enumerate() {
        let text = cue.text.trim();
        if text.is_empty() {
            return Err(MediaError::new(
                "empty_caption_cue",
                format!("Caption cue {} has no text", index + 1),
            ));
        }
        if text.len() > MAX_CAPTION_TEXT_BYTES
            || text.chars().any(|character| {
                character == '\0' || (character.is_control() && !matches!(character, '\n' | '\t'))
            })
        {
            return Err(MediaError::new(
                "invalid_caption_text",
                format!("Caption cue {} contains invalid text", index + 1),
            ));
        }
        if cue.start_us < 0 || cue.end_us <= cue.start_us || cue.end_us > duration_us {
            return Err(MediaError::new(
                "caption_out_of_bounds",
                format!("Caption cue {} falls outside the source clock", index + 1),
            ));
        }
        if cue.start_us < previous_start || cue.end_us < previous_end {
            return Err(MediaError::new(
                "caption_timing_not_monotonic",
                format!("Caption cue {} moves backward in source time", index + 1),
            ));
        }
        if cue.end_us - cue.start_us > 120_000_000 {
            return Err(MediaError::new(
                "caption_duration_implausible",
                format!("Caption cue {} is longer than two minutes", index + 1),
            ));
        }
        if index == 0 {
            union_start = cue.start_us;
            union_end = cue.end_us;
        } else if cue.start_us < union_end {
            overlap_count = overlap_count.saturating_add(1);
            union_end = union_end.max(cue.end_us);
        } else {
            spoken_span_us = spoken_span_us.saturating_add(union_end - union_start);
            union_start = cue.start_us;
            union_end = cue.end_us;
        }
        previous_start = cue.start_us;
        previous_end = cue.end_us;
        hasher.update(cue.id.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        hasher.update(cue.start_us.to_le_bytes());
        hasher.update(cue.end_us.to_le_bytes());
        hasher.update(text.as_bytes());
        hasher.update([0xff]);
    }
    spoken_span_us = spoken_span_us.saturating_add(union_end - union_start);
    Ok(CaptionValidation {
        cues: cues.to_vec(),
        duration_us,
        spoken_span_us,
        preserved_gap_us: duration_us.saturating_sub(spoken_span_us),
        overlap_count,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "soundar-media-{label}-{}-{sequence}",
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
    fn explicit_non_executable_tool_is_rejected_without_path_fallback() {
        let root = TestDirectory::new("discovery");
        let candidate = root.0.join("ffmpeg");
        fs::write(&candidate, "#!/bin/sh\necho 'ffmpeg version test'\n").expect("write candidate");
        let mut overrides = BTreeMap::new();
        overrides.insert(MediaToolKind::Ffmpeg, candidate);
        let context = DiscoveryContext {
            path: Some(OsString::from("/usr/bin")),
            home: None,
            overrides,
            run_nvenc_smoke: false,
        };
        let status = discover_media_runtime_with(&context);
        assert!(!status.ffmpeg.available);
        assert!(status.ffmpeg.configured);
        assert!(status
            .ffmpeg
            .diagnostic
            .as_deref()
            .is_some_and(|value| value.contains("not a regular executable")));
    }

    #[test]
    fn video_model_weights_are_found_by_role_not_by_exact_filename() {
        let root = TestDirectory::new("clip-models");
        // The community publishes these weights under many names and quantizations.
        for name in [
            "minimax_h3_fl2va_turbo_Q3_K_M.gguf",
            "minimax_h3_fl2va_pruned-Q2_K.gguf",
            "qwen3vl_32b_minimax_h3-Q2_K_M.gguf",
            "minimax_h3_video_vae_fp16.safetensors",
            "minimax_h3_audio_vae_fp32.safetensors",
            "notes.txt",
        ] {
            fs::write(root.0.join(name), vec![0_u8; name.len() * 16]).expect("write weight");
        }
        let models = resolve_clip_models(&root.0).expect("resolve weights");
        // The distilled checkpoint wins even though the undistilled one is smaller: soundAr's
        // fixed settings produce black frames on an undistilled model rather than failing.
        assert!(models.denoiser.to_string_lossy().contains("turbo"));
        // The text encoder must never be mistaken for the denoiser: both are .gguf.
        assert!(models.text_encoder.to_string_lossy().contains("qwen"));
        assert!(models.video_vae.to_string_lossy().contains("video_vae"));
        assert!(models.audio_vae.is_some());
    }

    #[test]
    fn a_reference_only_checkpoint_is_not_accepted_as_the_denoiser() {
        let root = TestDirectory::new("clip-models-ref");
        // Ref2VA cannot be driven from a first frame, so it must not silently stand in.
        for name in [
            "minimax_h3_ref2va_pruned-Q3_K.gguf",
            "qwen3vl_32b_minimax_h3-Q2_K_M.gguf",
            "minimax_h3_video_vae_fp16.safetensors",
        ] {
            fs::write(root.0.join(name), vec![0_u8; 32]).expect("write weight");
        }
        let error = resolve_clip_models(&root.0).expect_err("no usable denoiser");
        assert_eq!(error.code, "clip_model_missing");

        // An undistilled first-and-last-frame checkpoint is refused for the same reason.
        let undistilled = TestDirectory::new("clip-models-undistilled");
        for name in [
            "minimax_h3_fl2va_pruned-Q2_K.gguf",
            "qwen3vl_32b_minimax_h3-Q2_K_M.gguf",
            "minimax_h3_video_vae_fp16.safetensors",
        ] {
            fs::write(undistilled.0.join(name), vec![0_u8; 32]).expect("write weight");
        }
        let error = resolve_clip_models(&undistilled.0).expect_err("undistilled refused");
        assert!(
            error.message.to_lowercase().contains("distilled"),
            "{error:?}"
        );
    }

    #[test]
    fn a_missing_video_vae_is_named_rather_than_guessed_at() {
        let root = TestDirectory::new("clip-models-partial");
        for name in [
            "minimax_h3_fl2va_turbo_Q3_K_M.gguf",
            "qwen3vl_32b_minimax_h3-Q2_K_M.gguf",
        ] {
            fs::write(root.0.join(name), vec![0_u8; 32]).expect("write weight");
        }
        let error = resolve_clip_models(&root.0).expect_err("no video vae");
        assert!(
            error.message.to_lowercase().contains("video vae"),
            "{error:?}"
        );
        // Audio is optional, so its absence alone is not an error.
        let full = TestDirectory::new("clip-models-noaudio");
        for name in [
            "minimax_h3_fl2va_turbo_Q3_K_M.gguf",
            "qwen3vl_32b_minimax_h3-Q2_K_M.gguf",
            "minimax_h3_video_vae_fp16.safetensors",
        ] {
            fs::write(full.0.join(name), vec![0_u8; 32]).expect("write weight");
        }
        let models = resolve_clip_models(&full.0).expect("resolve without audio");
        assert!(models.audio_vae.is_none());
    }

    #[test]
    fn configured_executable_is_reported_with_version() {
        let root = TestDirectory::new("configured");
        let candidate = root.0.join("node");
        let mut file = fs::File::create(&candidate).expect("create candidate");
        file.write_all(b"#!/bin/sh\necho 'v99.2.1'\n")
            .expect("write candidate");
        // Close before making it executable: while any descriptor is open for writing, executing
        // the file fails with ETXTBSY.
        drop(file);
        make_test_executable(&candidate);
        let mut overrides = BTreeMap::new();
        overrides.insert(MediaToolKind::Node, candidate.clone());
        let context = DiscoveryContext {
            path: Some(OsString::new()),
            home: None,
            overrides,
            run_nvenc_smoke: false,
        };
        let status = discover_tool(MediaToolKind::Node, &context);
        assert!(status.available, "{status:?}");
        assert!(status.configured);
        assert_eq!(status.version.as_deref(), Some("v99.2.1"));
        assert_eq!(status.path, Some(fs::canonicalize(candidate).unwrap()));
    }

    #[test]
    fn managed_soundar_runtime_is_discovered_without_desktop_path_inheritance() {
        let home = TestDirectory::new("managed-yt-dlp");
        let candidate = home
            .0
            .join(".local/share/soundar/runtimes/yt-dlp-test/bin/yt-dlp");
        fs::create_dir_all(candidate.parent().expect("runtime bin directory"))
            .expect("create managed runtime");
        let mut file = fs::File::create(&candidate).expect("create managed yt-dlp");
        file.write_all(b"#!/bin/sh\necho '2026.06.09'\n")
            .expect("write managed yt-dlp");
        drop(file);
        make_test_executable(&candidate);

        let context = DiscoveryContext {
            path: Some(OsString::new()),
            home: Some(home.0.clone()),
            overrides: BTreeMap::new(),
            run_nvenc_smoke: false,
        };
        let status = discover_tool(MediaToolKind::YtDlp, &context);
        assert!(status.available, "{status:?}");
        assert_eq!(status.version.as_deref(), Some("2026.06.09"));
        assert_eq!(status.path, Some(fs::canonicalize(candidate).unwrap()));
    }

    #[test]
    fn command_capture_drains_large_streams_with_strict_retention_limits() {
        let args = [
            OsString::from("-c"),
            OsString::from("head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2"),
        ];
        let output = run_bounded_command_with_limits(
            Path::new("/bin/sh"),
            &args,
            Duration::from_secs(5),
            4_096,
            2_048,
        )
        .expect("bounded high-volume helper");
        assert!(output.status_success);
        assert_eq!(output.stdout.len(), 4_096);
        assert_eq!(output.stderr.len(), 2_048);
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
    }

    #[test]
    fn command_timeout_kills_the_process_group_and_completes_pipe_drain() {
        let args = [
            OsString::from("-c"),
            OsString::from(
                "while :; do printf '0123456789abcdef'; printf 'fedcba9876543210' >&2; done",
            ),
        ];
        let started = Instant::now();
        let error = run_bounded_command_with_limits(
            Path::new("/bin/sh"),
            &args,
            Duration::from_millis(100),
            128,
            128,
        )
        .expect_err("non-terminating helper must time out");
        assert_eq!(error.code, "media_process_timeout");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn command_capture_does_not_hang_when_a_descendant_keeps_pipes_open() {
        let root = TestDirectory::new("inherited-pipe");
        let pid_path = root.0.join("descendant.pid");
        let script = format!(
            "(while :; do printf 'continuous-output'; done) & descendant=$!; printf '%s' \"$descendant\" > '{}'; exit 0",
            pid_path.display()
        );
        let args = [OsString::from("-c"), OsString::from(script)];
        let started = Instant::now();
        let error = run_bounded_command_with_limits(
            Path::new("/bin/sh"),
            &args,
            Duration::from_secs(5),
            128,
            128,
        )
        .expect_err("inherited output descriptors must fail closed");
        assert_eq!(error.code, "media_process_failed");
        assert!(started.elapsed() < Duration::from_secs(3));

        let descendant = fs::read_to_string(&pid_path)
            .expect("read descendant pid")
            .parse::<i32>()
            .expect("parse descendant pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut descendant_is_gone = false;
        while Instant::now() < deadline {
            let status = unsafe { libc::kill(descendant, 0) };
            if status < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                descendant_is_gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !descendant_is_gone {
            // Best-effort cleanup keeps a failing regression test from leaving a hot loop.
            unsafe {
                libc::kill(descendant, libc::SIGKILL);
            }
        }
        assert!(
            descendant_is_gone,
            "owned descendant survived process-group cleanup"
        );
    }

    #[test]
    fn probe_rejects_oversized_sparse_sources_before_starting_ffprobe() {
        let root = TestDirectory::new("source-quota");
        let source = root.0.join("oversized.mp4");
        let file = fs::File::create(&source).expect("create sparse source");
        file.set_len(MAX_MEDIA_SOURCE_BYTES + 1)
            .expect("extend sparse source");
        drop(file);
        let error = probe_media(&source, Path::new("/bin/false"))
            .expect_err("source quota must precede helper execution");
        assert_eq!(error.code, "media_source_too_large");
    }

    #[test]
    fn local_input_policy_rejects_renamed_network_and_playlist_manifests() {
        let root = TestDirectory::new("manifest-policy");
        for (name, contents) in [
            (
                "renamed-hls.mp4",
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1280000\nhttps://private.example/live.m3u8\n",
            ),
            (
                "renamed-dash.webm",
                "<?xml version=\"1.0\"?><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"></MPD>",
            ),
            (
                "renamed-concat.mov",
                "ffconcat version 1.0\nfile '/etc/passwd'\n",
            ),
            (
                "renamed-sdp.wav",
                "v=0\no=- 1 1 IN IP4 127.0.0.1\nc=IN IP4 127.0.0.1\nm=audio 5004 RTP/AVP 0\n",
            ),
            (
                "renamed-playlist.mp3",
                "[playlist]\nFile1=https://private.example/audio.mp3\n",
            ),
            (
                "renamed-xspf.ogg",
                "<playlist xmlns=\"http://xspf.org/ns/0/\"><trackList /></playlist>",
            ),
        ] {
            let path = root.0.join(name);
            fs::write(&path, contents).expect("write renamed manifest");
            let error = validate_local_media_source(&path)
                .expect_err("renamed manifest must fail before FFmpeg/FFprobe");
            assert_eq!(error.code, "unsafe_media_manifest", "source {name}");
        }
    }

    #[test]
    fn local_input_args_apply_file_only_protocol_and_safe_demuxer_allowlists() {
        let root = TestDirectory::new("local-input-args");
        let source = root.0.join("ordinary.mp4");
        fs::write(&source, b"ordinary non-manifest bytes").unwrap();
        let args = local_media_input_args(&source).expect("build guarded input arguments");
        assert_eq!(args[0], OsString::from("-protocol_whitelist"));
        assert_eq!(args[1], OsString::from("file"));
        assert_eq!(args[2], OsString::from("-format_whitelist"));
        assert_eq!(args[4], OsString::from("-i"));
        assert_eq!(args[5], fs::canonicalize(source).unwrap().into_os_string());
        let formats = LOCAL_MEDIA_FORMAT_WHITELIST
            .split(',')
            .collect::<HashSet<_>>();
        for forbidden in [
            "concat",
            "dash",
            "ffmetadata",
            "hls",
            "image2",
            "imf",
            "live_flv",
            "rtsp",
            "sdp",
            "smil",
            "xspf",
        ] {
            assert!(!formats.contains(forbidden), "demuxer {forbidden}");
        }
    }

    #[test]
    fn probe_rejects_an_unsafe_reported_demuxer_even_if_the_helper_ignores_options() {
        let root = TestDirectory::new("reported-demuxer");
        let source = root.0.join("ordinary.mp4");
        fs::write(&source, b"ordinary non-manifest bytes").unwrap();
        let fake_ffprobe = root.0.join("ffprobe");
        fs::write(
            &fake_ffprobe,
            "#!/bin/sh\nprintf '%s\\n' '{\"format\":{\"format_name\":\"hls\",\"duration\":\"1\"},\"streams\":[{\"index\":0,\"codec_type\":\"video\"}]}'\n",
        )
        .unwrap();
        make_test_executable(&fake_ffprobe);
        let error = probe_media(&source, &fake_ffprobe)
            .expect_err("unsafe reported demuxer must fail closed");
        assert_eq!(error.code, "unsafe_media_manifest");
    }

    #[test]
    fn exact_decimal_clock_conversion_does_not_use_floating_point() {
        assert_eq!(decimal_seconds_to_us("12.3456784"), Some(12_345_678));
        assert_eq!(decimal_seconds_to_us("12.3456785"), Some(12_345_679));
        assert_eq!(decimal_seconds_to_us("-0.250000"), Some(-250_000));
        assert_eq!(decimal_seconds_to_us("N/A"), None);
    }

    #[test]
    fn youtube_urls_are_canonicalized_and_playlists_are_rejected() {
        let validated = validate_import_url("https://youtu.be/dQw4w9WgXcQ?t=2#watch")
            .expect("valid direct URL");
        assert_eq!(
            validated.canonical,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(validated.provider, ImportProvider::YouTube);
        assert!(validated.rights_confirmation_required);
        let error = validate_import_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&l%69st=PL123")
            .expect_err("encoded playlist key must be rejected");
        assert_eq!(error.code, "playlist_not_allowed");
    }

    #[test]
    fn import_url_rejects_credentials_private_hosts_and_multiple_values() {
        assert_eq!(
            validate_import_url("https://person@example.com/video")
                .unwrap_err()
                .code,
            "invalid_url"
        );
        assert_eq!(
            validate_import_url("https://127.0.0.1/video")
                .unwrap_err()
                .code,
            "unsafe_url_host"
        );
        assert_eq!(
            validate_import_url("https://example.com/a https://example.com/b")
                .unwrap_err()
                .code,
            "invalid_url"
        );
    }

    #[test]
    fn import_url_and_dns_policy_reject_all_reserved_address_classes() {
        for address in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.0.9",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            let error = validate_import_url(&format!("https://{address}/video"))
                .expect_err("reserved literal must be rejected syntactically");
            assert_eq!(error.code, "unsafe_url_host", "address {address}");
        }

        for address in [
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001:2::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(
                !is_public_network_address(address.parse().expect("test IP")),
                "address {address}"
            );
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(
                is_public_network_address(address.parse().expect("test IP")),
                "address {address}"
            );
        }
    }

    #[test]
    fn dns_policy_rejects_a_mixed_public_and_private_answer_set() {
        let error = validate_public_destination_addresses(
            "media.example.com",
            vec!["1.1.1.1".parse().unwrap(), "127.0.0.1".parse().unwrap()],
        )
        .expect_err("one unsafe answer must reject the entire destination");
        assert_eq!(error.code, "unsafe_url_destination");

        let public = validate_public_destination_addresses(
            "media.example.com",
            vec![
                "1.1.1.1".parse().unwrap(),
                "2606:4700:4700::1111".parse().unwrap(),
            ],
        )
        .expect("entirely public answer set");
        assert_eq!(public.len(), 2);
    }

    #[test]
    fn preflight_rejects_a_forged_post_validation_contract_without_dns() {
        let mut validated =
            validate_import_url("https://media.example.com/video").expect("valid syntax");
        validated.canonical = "https://other.example.com/video".to_string();
        let error = preflight_import_url_destination(&validated)
            .expect_err("canonical URL mutation must fail before resolution");
        assert_eq!(error.code, "invalid_url_preflight");
    }

    #[test]
    fn protected_proxy_requires_auth_and_rejects_private_and_http_redirects() {
        let proxy = PublicHttpsProxy::start().expect("start protected proxy");
        let authorization = proxy_test_authorization(&proxy);

        let unauthenticated = proxy_test_request(
            &proxy,
            "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n",
        );
        assert!(unauthenticated.starts_with("HTTP/1.1 407"));

        let private_redirect = proxy_test_request(
            &proxy,
            &format!(
                "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\nProxy-Authorization: {authorization}\r\n\r\n"
            ),
        );
        assert!(private_redirect.starts_with("HTTP/1.1 403"));

        let http_downgrade = proxy_test_request(
            &proxy,
            &format!(
                "GET http://127.0.0.1/secret HTTP/1.1\r\nHost: 127.0.0.1\r\nProxy-Authorization: {authorization}\r\n\r\n"
            ),
        );
        assert!(http_downgrade.starts_with("HTTP/1.1 405"));
    }

    #[test]
    fn protected_proxy_bounds_connect_headers_and_redacts_credentials_in_debug() {
        let proxy = PublicHttpsProxy::start().expect("start protected proxy");
        let authorization = proxy_test_authorization(&proxy);
        let oversized = proxy_test_request(
            &proxy,
            &format!(
                "CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: {authorization}\r\nX-Fill: {}\r\n\r\n",
                "x".repeat(MAX_PROXY_CONNECT_HEADER_BYTES)
            ),
        );
        assert!(oversized.starts_with("HTTP/1.1 400"));
        let debug = format!("{proxy:?}");
        assert!(!debug.contains(proxy.url()));
        assert!(!debug.contains(&authorization));
    }

    #[test]
    fn proxy_tunnel_relays_both_directions_with_bounded_buffers() {
        let (mut user, proxy_client) = connected_tcp_pair();
        let (proxy_upstream, mut origin) = connected_tcp_pair();
        user.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        origin
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let relay_stop = Arc::clone(&stop);
        let relay =
            thread::spawn(move || relay_proxy_tunnel(proxy_client, proxy_upstream, relay_stop));

        user.write_all(b"request through proxy").unwrap();
        let mut request = [0u8; 21];
        origin.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"request through proxy");

        origin.write_all(b"response through proxy").unwrap();
        let mut response = [0u8; 22];
        user.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"response through proxy");

        stop.store(true, Ordering::Release);
        let _ = user.shutdown(std::net::Shutdown::Both);
        let _ = origin.shutdown(std::net::Shutdown::Both);
        relay.join().expect("join tunnel relay");
    }

    #[test]
    fn base64_encoder_matches_proxy_basic_auth_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"soundar:token"), "c291bmRhcjp0b2tlbg==");
    }

    #[test]
    fn caption_validation_preserves_source_gaps_and_allows_rolling_overlap() {
        let cues = vec![
            CaptionCueInput {
                id: Some("one".into()),
                start_us: 1_000_000,
                end_us: 3_000_000,
                text: "First".into(),
            },
            CaptionCueInput {
                id: Some("two".into()),
                start_us: 2_500_000,
                end_us: 4_000_000,
                text: "First second".into(),
            },
            CaptionCueInput {
                id: None,
                start_us: 6_000_000,
                end_us: 7_000_000,
                text: "Third".into(),
            },
        ];
        let validation = validate_caption_cues(&cues, 10_000_000).expect("valid captions");
        assert_eq!(validation.spoken_span_us, 4_000_000);
        assert_eq!(validation.preserved_gap_us, 6_000_000);
        assert_eq!(validation.overlap_count, 1);
        assert_eq!(validation.cues, cues);
    }

    #[test]
    fn caption_validation_rejects_backward_and_out_of_bounds_timing() {
        let backward = [
            CaptionCueInput {
                id: None,
                start_us: 2_000_000,
                end_us: 3_000_000,
                text: "Later".into(),
            },
            CaptionCueInput {
                id: None,
                start_us: 1_000_000,
                end_us: 4_000_000,
                text: "Backward".into(),
            },
        ];
        assert_eq!(
            validate_caption_cues(&backward, 5_000_000)
                .unwrap_err()
                .code,
            "caption_timing_not_monotonic"
        );
        let out_of_bounds = [CaptionCueInput {
            id: None,
            start_us: 1,
            end_us: 5_000_001,
            text: "Too late".into(),
        }];
        assert_eq!(
            validate_caption_cues(&out_of_bounds, 5_000_000)
                .unwrap_err()
                .code,
            "caption_out_of_bounds"
        );
    }

    #[test]
    fn ffprobe_smoke_reads_a_locally_generated_fixture_when_tools_are_available() {
        let Some(ffmpeg) = find_in_path("ffmpeg") else {
            return;
        };
        let Some(ffprobe) = find_in_path("ffprobe") else {
            return;
        };
        let root = TestDirectory::new("probe-smoke");
        let fixture = root.0.join("fixture.mp4");
        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=96x64:r=10:d=0.3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.3",
                "-shortest",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-y",
            ])
            .arg(&fixture)
            .status()
            .expect("run local FFmpeg fixture generation");
        if !status.success() {
            return;
        }
        let probe = probe_media(&fixture, &ffprobe).expect("probe generated fixture");
        assert!(probe.duration_us >= 200_000);
        assert!(probe.primary_video_stream.is_some());
        assert!(probe.primary_audio_stream.is_some());
        assert_eq!(probe.path, fs::canonicalize(fixture).unwrap());
    }

    fn find_in_path(name: &str) -> Option<PathBuf> {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join(name))
            .find(|path| is_executable_file(path))
    }

    fn proxy_test_authorization(proxy: &PublicHttpsProxy) -> String {
        let credentials = proxy
            .url()
            .strip_prefix("http://")
            .and_then(|value| value.split_once('@'))
            .map(|(credentials, _)| credentials)
            .expect("proxy URL credentials");
        format!("Basic {}", encode_base64(credentials.as_bytes()))
    }

    fn proxy_test_request(proxy: &PublicHttpsProxy, request: &str) -> String {
        let mut client = TcpStream::connect(proxy.address).expect("connect to proxy");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(request.as_bytes()).expect("write request");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read response");
        response
    }

    fn connected_tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind test listener");
        let client = TcpStream::connect(listener.local_addr().unwrap()).expect("connect test pair");
        let (server, _) = listener.accept().expect("accept test pair");
        (client, server)
    }

    /// Make a script executable and wait until it can actually be executed.
    ///
    /// Sibling tests in this binary spawn processes. A child forked while this file was still open
    /// for writing inherits that descriptor until it execs, and executing the file in that window
    /// fails with ETXTBSY - a transient scheduling accident that says nothing about the code under
    /// test. Running it once here closes that window before the assertions depend on it. Every
    /// script this helper is used on is trivial and side-effect free, so running it is free.
    fn make_test_executable(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Command::new(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(_) => return,
                Err(error)
                    if error.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!(
                    "test executable {} never became runnable: {error}",
                    path.display()
                ),
            }
        }
    }
}
