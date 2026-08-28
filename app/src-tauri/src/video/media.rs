use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    env,
    ffi::{OsStr, OsString},
    fmt, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_PROBE_JSON_BYTES: usize = 8 * 1024 * 1024;
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IMPORT_URL_BYTES: usize = 4_096;
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
        }
    }

    fn environment_keys(self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["SOUNDAR_FFMPEG_BIN", "FFMPEG_BIN"],
            Self::Ffprobe => &["SOUNDAR_FFPROBE_BIN", "FFPROBE_BIN"],
            Self::YtDlp => &["SOUNDAR_YT_DLP_BIN", "YT_DLP_BIN"],
            Self::Node => &["SOUNDAR_NODE_BIN", "NODE_BIN"],
            Self::Deno => &["SOUNDAR_DENO_BIN", "DENO_BIN"],
            Self::FasterWhisper => &["SOUNDAR_FASTER_WHISPER_BIN", "FASTER_WHISPER_BIN"],
            Self::WhisperCpp => &[
                "SOUNDAR_WHISPER_CPP_BIN",
                "WHISPER_CPP_BIN",
                "WHISPER_CLI_BIN",
            ],
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

fn all_tool_kinds() -> [MediaToolKind; 7] {
    [
        MediaToolKind::Ffmpeg,
        MediaToolKind::Ffprobe,
        MediaToolKind::YtDlp,
        MediaToolKind::Node,
        MediaToolKind::Deno,
        MediaToolKind::FasterWhisper,
        MediaToolKind::WhisperCpp,
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

    MediaRuntimeStatus {
        ffmpeg,
        ffprobe,
        yt_dlp,
        node,
        deno,
        faster_whisper,
        whisper_cpp,
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
            MediaToolKind::Node | MediaToolKind::YtDlp => vec![
                home.join(".nvm/versions/node"),
                home.join(".local/share/fnm/node-versions"),
                home.join(".asdf/installs/nodejs"),
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
        _ => &["--version"],
    };
    let mut output = Command::new(path)
        .args(version_args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not run {}: {error}", path.display()))?;
    if !output.status.success() && kind == MediaToolKind::WhisperCpp {
        output = Command::new(path)
            .arg("--help")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    }
    if !output.status.success() {
        return Err(format!(
            "{} version check exited with {}",
            path.display(),
            output.status
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
    let encoders = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
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
    let filters = Command::new(ffmpeg)
        .args(["-hide_banner", "-filters"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
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
    let hardware = Command::new(ffmpeg)
        .args(["-hide_banner", "-hwaccels"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    for accelerator in ["cuda", "vaapi", "qsv", "vulkan"] {
        if hardware.lines().any(|line| line.trim() == accelerator) {
            capabilities.push(format!("hwaccel_{accelerator}"));
        }
    }
    capabilities
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
    Command::new(ffmpeg)
        .args([
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
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
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

    let args = [
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-print_format"),
        OsString::from("json"),
        OsString::from("-show_format"),
        OsString::from("-show_streams"),
        OsString::from("-show_chapters"),
        source.as_os_str().to_os_string(),
    ];
    let output = run_bounded_command(&executable, &args, FFPROBE_TIMEOUT)?;
    if !output.status_success {
        return Err(
            MediaError::new("ffprobe_failed", "FFprobe rejected the selected media")
                .detail(truncate_diagnostic(&output.stderr, 2_000)),
        );
    }
    if output.stdout.len() > MAX_PROBE_JSON_BYTES {
        return Err(MediaError::new(
            "ffprobe_output_too_large",
            "FFprobe returned an unexpectedly large response",
        ));
    }
    let raw: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        MediaError::new("ffprobe_invalid_json", "FFprobe returned invalid metadata")
            .detail(error.to_string())
    })?;
    parse_probe_value(&source, metadata.len(), &raw)
}

struct BoundedCommandOutput {
    status_success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_bounded_command(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<BoundedCommandOutput, MediaError> {
    use std::io::Read;
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
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let process_group = -(child.id() as i32);
                unsafe {
                    libc::kill(process_group, libc::SIGKILL);
                }
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(MediaError::new(
                    "media_process_timeout",
                    "The media inspection process timed out",
                )
                .retryable(true));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(MediaError::new(
                    "media_process_failed",
                    "The media inspection process could not be monitored",
                )
                .detail(error.to_string())
                .retryable(true));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| MediaError::new("media_process_failed", "FFprobe output reader failed"))?
        .map_err(|error| {
            MediaError::new("media_process_failed", "FFprobe output could not be read")
                .detail(error.to_string())
        })?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| MediaError::new("media_process_failed", "FFprobe error reader failed"))?
        .map_err(|error| {
            MediaError::new(
                "media_process_failed",
                "FFprobe diagnostics could not be read",
            )
            .detail(error.to_string())
        })?;
    Ok(BoundedCommandOutput {
        status_success: status.success(),
        stdout,
        stderr,
    })
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
        if address.is_private()
            || address.is_loopback()
            || address.is_link_local()
            || address.is_unspecified()
            || address.is_broadcast()
            || address.octets()[0] == 0
            || address.octets()[0] >= 224
        {
            return Err(MediaError::new(
                "unsafe_url_host",
                "Private and local media hosts are not supported",
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
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
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
    fn configured_executable_is_reported_with_version() {
        let root = TestDirectory::new("configured");
        let candidate = root.0.join("node");
        let mut file = fs::File::create(&candidate).expect("create candidate");
        file.write_all(b"#!/bin/sh\necho 'v99.2.1'\n")
            .expect("write candidate");
        let mut permissions = file.metadata().expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&candidate, permissions).expect("make executable");
        drop(file);
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
}
