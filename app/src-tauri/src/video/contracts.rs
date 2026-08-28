use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const VIDEO_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const SHA256_HEX_LENGTH: usize = 64;
pub const NORMALIZED_BASIS_POINTS: i32 = 10_000;
pub const MAX_SOURCE_DURATION_US: i64 = 6 * 60 * 60 * 1_000_000;
pub const MAX_TIMELINE_DURATION_US: i64 = MAX_SOURCE_DURATION_US;

pub type VideoResult<T> = Result<T, VideoError>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoErrorCode {
    UnsupportedSchema,
    InvalidIdentifier,
    InvalidTimestamp,
    InvalidFrameRate,
    InvalidAsset,
    MissingRightsConfirmation,
    InvalidRightsConfirmation,
    InvalidTranscript,
    InvalidCandidate,
    InvalidScene,
    InvalidTrack,
    InvalidGap,
    InvalidCaption,
    InvalidLayout,
    InvalidAudioMix,
    InvalidNarration,
    InvalidArtifact,
    InvalidRevision,
    DuplicateId,
    MissingReference,
    TimelineOverlap,
    TimelineGap,
    DurationMismatch,
    ArithmeticOverflow,
    InvalidCacheInput,
    InvalidResourceRequest,
    ResourceUnavailable,
    JobAlreadyActive,
}

impl VideoErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "video.unsupported_schema",
            Self::InvalidIdentifier => "video.invalid_identifier",
            Self::InvalidTimestamp => "video.invalid_timestamp",
            Self::InvalidFrameRate => "video.invalid_frame_rate",
            Self::InvalidAsset => "video.invalid_asset",
            Self::MissingRightsConfirmation => "video.missing_rights_confirmation",
            Self::InvalidRightsConfirmation => "video.invalid_rights_confirmation",
            Self::InvalidTranscript => "video.invalid_transcript",
            Self::InvalidCandidate => "video.invalid_candidate",
            Self::InvalidScene => "video.invalid_scene",
            Self::InvalidTrack => "video.invalid_track",
            Self::InvalidGap => "video.invalid_gap",
            Self::InvalidCaption => "video.invalid_caption",
            Self::InvalidLayout => "video.invalid_layout",
            Self::InvalidAudioMix => "video.invalid_audio_mix",
            Self::InvalidNarration => "video.invalid_narration",
            Self::InvalidArtifact => "video.invalid_artifact",
            Self::InvalidRevision => "video.invalid_revision",
            Self::DuplicateId => "video.duplicate_id",
            Self::MissingReference => "video.missing_reference",
            Self::TimelineOverlap => "video.timeline_overlap",
            Self::TimelineGap => "video.timeline_gap",
            Self::DurationMismatch => "video.duration_mismatch",
            Self::ArithmeticOverflow => "video.arithmetic_overflow",
            Self::InvalidCacheInput => "video.invalid_cache_input",
            Self::InvalidResourceRequest => "video.invalid_resource_request",
            Self::ResourceUnavailable => "video.resource_unavailable",
            Self::JobAlreadyActive => "video.job_already_active",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoError {
    pub code: VideoErrorCode,
    pub message: String,
    pub field: Option<String>,
    pub retryable: bool,
    pub details: Option<Value>,
}

impl VideoError {
    pub fn new(code: VideoErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
            retryable: false,
            details: None,
        }
    }

    pub fn at(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn stable_code(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(
                formatter,
                "{} at {}: {}",
                self.code.as_str(),
                field,
                self.message
            ),
            None => write!(formatter, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for VideoError {}

pub trait Validate {
    fn validate(&self) -> VideoResult<()>;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Microseconds(pub i64);

impl Microseconds {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, other: Self) -> VideoResult<Self> {
        self.0.checked_add(other.0).map(Self).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "microsecond addition overflowed",
            )
        })
    }

    pub fn is_non_negative(self) -> bool {
        self.0 >= 0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start_us: Microseconds,
    pub end_us: Microseconds,
}

impl TimeRange {
    pub fn new(start_us: i64, end_us: i64) -> VideoResult<Self> {
        let range = Self {
            start_us: Microseconds(start_us),
            end_us: Microseconds(end_us),
        };
        range.validate()?;
        Ok(range)
    }

    pub fn duration(self) -> VideoResult<Microseconds> {
        self.end_us
            .0
            .checked_sub(self.start_us.0)
            .map(Microseconds)
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::ArithmeticOverflow,
                    "time range duration overflowed",
                )
            })
    }

    /// Returns whether a media sample belongs to this half-open range.
    ///
    /// Timeline samples use `[start_us, end_us)`, so two adjacent ranges never
    /// claim the same sample. Use [`Self::contains_endpoint`] only when mapping
    /// an edit boundary rather than locating media at an instant.
    pub fn contains(self, instant: Microseconds) -> bool {
        instant >= self.start_us && instant < self.end_us
    }

    /// Returns whether an edit boundary lies on or within this range.
    ///
    /// Endpoints are closed for exact range conversion: the exclusive end of a
    /// clip may be converted to the corresponding exclusive end in another
    /// clock without making that endpoint a sample owned by the clip.
    pub fn contains_endpoint(self, instant: Microseconds) -> bool {
        instant >= self.start_us && instant <= self.end_us
    }
}

impl Validate for TimeRange {
    fn validate(&self) -> VideoResult<()> {
        if self.start_us.0 < 0 || self.end_us.0 <= self.start_us.0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTimestamp,
                "a time range must have 0 <= start_us < end_us",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RationalFrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl RationalFrameRate {
    pub const FPS_24: Self = Self {
        numerator: 24,
        denominator: 1,
    };
    pub const FPS_30: Self = Self {
        numerator: 30,
        denominator: 1,
    };
    pub const FPS_30000_1001: Self = Self {
        numerator: 30_000,
        denominator: 1_001,
    };
}

impl Validate for RationalFrameRate {
    fn validate(&self) -> VideoResult<()> {
        if self.numerator == 0 || self.denominator == 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidFrameRate,
                "frame-rate numerator and denominator must be positive",
            ));
        }
        if self.numerator > 240_000 || self.denominator > 10_000 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidFrameRate,
                "frame rate is outside the supported range",
            ));
        }
        if gcd_u32(self.numerator, self.denominator) != 1 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidFrameRate,
                "frame rate must be stored as a reduced rational",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RationalRate {
    /// Source-clock units consumed for every `denominator` timeline units.
    pub numerator: u32,
    pub denominator: u32,
}

pub const MIN_PLAYBACK_RATE: RationalRate = RationalRate {
    numerator: 1,
    denominator: 8,
};
pub const MAX_PLAYBACK_RATE: RationalRate = RationalRate {
    numerator: 8,
    denominator: 1,
};

impl RationalRate {
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };
}

impl Validate for RationalRate {
    fn validate(&self) -> VideoResult<()> {
        if self.numerator == 0 || self.denominator == 0 {
            return Err(VideoError::new(
                VideoErrorCode::DurationMismatch,
                "playback-rate numerator and denominator must be positive",
            ));
        }
        if gcd_u32(self.numerator, self.denominator) != 1 {
            return Err(VideoError::new(
                VideoErrorCode::DurationMismatch,
                "playback rate must be stored as a reduced rational",
            ));
        }
        let numerator = u64::from(self.numerator);
        let denominator = u64::from(self.denominator);
        if numerator * u64::from(MIN_PLAYBACK_RATE.denominator)
            < u64::from(MIN_PLAYBACK_RATE.numerator) * denominator
            || numerator * u64::from(MAX_PLAYBACK_RATE.denominator)
                > u64::from(MAX_PLAYBACK_RATE.numerator) * denominator
        {
            return Err(VideoError::new(
                VideoErrorCode::DurationMismatch,
                "playback rate must be within the supported 1/8x..=8x range",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAssetKind {
    LocalVideo,
    LocalAudio,
    ImportedLink,
    SoundArSpeech,
    SoundArMusic,
    SoundArProject,
    Generated,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceKind {
    UserUpload,
    AuthorizedLink,
    ExistingSoundArArtifact,
    GeneratedLocally,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub kind: ProvenanceKind,
    pub original_uri: Option<String>,
    pub imported_at: String,
    pub producer: String,
    pub producer_version: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RightsBasis {
    Owned,
    Licensed,
    PublicDomain,
    CreativeCommons,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RightsConfirmation {
    pub id: String,
    pub source_uri: String,
    pub source_uri_sha256: String,
    pub basis: RightsBasis,
    pub confirmation_text: String,
    pub confirmed_by: String,
    pub confirmed_at: String,
    /// Always true: a confirmation is bound to exactly one normalized source URL.
    pub single_source_only: bool,
}

impl Validate for RightsConfirmation {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "rights.id")?;
        validate_nonempty(&self.source_uri, "rights.source_uri", 4_096)?;
        validate_sha256(
            &self.source_uri_sha256,
            "rights.source_uri_sha256",
            VideoErrorCode::InvalidRightsConfirmation,
        )?;
        if self.source_uri_sha256 != format!("{:x}", Sha256::digest(self.source_uri.as_bytes())) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidRightsConfirmation,
                "source_uri_sha256 does not match the exact confirmed source URI",
            )
            .at("rights.source_uri_sha256"));
        }
        validate_nonempty(&self.confirmation_text, "rights.confirmation_text", 4_096)?;
        validate_nonempty(&self.confirmed_by, "rights.confirmed_by", 256)?;
        validate_timestamp_text(&self.confirmed_at, "rights.confirmed_at")?;
        if !self.single_source_only {
            return Err(VideoError::new(
                VideoErrorCode::InvalidRightsConfirmation,
                "rights confirmations must be scoped to exactly one source",
            )
            .at("rights.single_source_only"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaProbe {
    pub duration_us: Microseconds,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<RationalFrameRate>,
    pub has_video: bool,
    pub has_audio: bool,
    pub format_name: String,
}

impl Validate for MediaProbe {
    fn validate(&self) -> VideoResult<()> {
        if !(1..=MAX_SOURCE_DURATION_US).contains(&self.duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "media duration must be positive and no greater than 6 hours",
            )
            .at("media.duration_us"));
        }
        if !self.has_video && !self.has_audio {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "media must have at least one audio or video stream",
            ));
        }
        match (self.width, self.height, self.has_video) {
            (Some(width), Some(height), true) if width > 0 && height > 0 => {}
            (None, None, false) => {}
            _ => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    "video dimensions must both be present and positive",
                ))
            }
        }
        if let Some(frame_rate) = self.frame_rate {
            frame_rate.validate()?;
        }
        validate_nonempty(&self.format_name, "media.format_name", 128)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAsset {
    pub id: String,
    pub kind: SourceAssetKind,
    /// A path within the application's managed artifact store.
    pub managed_path: String,
    pub sha256: String,
    pub probe: MediaProbe,
    pub provenance: Provenance,
    pub rights_confirmation_id: Option<String>,
}

impl Validate for SourceAsset {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "source_assets.id")?;
        validate_managed_path(&self.managed_path, "source_assets.managed_path")?;
        validate_sha256(
            &self.sha256,
            "source_assets.sha256",
            VideoErrorCode::InvalidAsset,
        )?;
        self.probe.validate()?;
        validate_timestamp_text(&self.provenance.imported_at, "provenance.imported_at")?;
        validate_nonempty(&self.provenance.producer, "provenance.producer", 256)?;
        if matches!(self.kind, SourceAssetKind::ImportedLink)
            && self.rights_confirmation_id.is_none()
        {
            return Err(VideoError::new(
                VideoErrorCode::MissingRightsConfirmation,
                "an imported link requires a per-source rights confirmation",
            )
            .at("source_assets.rights_confirmation_id"));
        }
        if let Some(id) = &self.rights_confirmation_id {
            validate_identifier(id, "source_assets.rights_confirmation_id")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptTimingSource {
    FasterWhisper,
    WhisperCpp,
    SoundArWhisper,
    ValidatedExternal,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptWord {
    pub id: String,
    pub range: TimeRange,
    pub text: String,
    pub speaker_id: Option<String>,
    pub confidence_milli: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSegment {
    pub id: String,
    pub range: TimeRange,
    pub text: String,
    pub speaker_id: Option<String>,
    pub word_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptVersion {
    pub id: String,
    pub source_asset_id: String,
    pub source_clock_duration_us: Microseconds,
    pub language: Option<String>,
    pub timing_source: TranscriptTimingSource,
    pub preserved_source_gaps: bool,
    pub segments: Vec<TranscriptSegment>,
    pub words: Vec<TranscriptWord>,
    pub content_sha256: String,
    pub created_at: String,
}

impl Validate for TranscriptVersion {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "transcript.id")?;
        validate_identifier(&self.source_asset_id, "transcript.source_asset_id")?;
        if !(1..=MAX_SOURCE_DURATION_US).contains(&self.source_clock_duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "transcript source-clock duration must be positive and no greater than 6 hours",
            )
            .at("transcript.source_clock_duration_us"));
        }
        if !self.preserved_source_gaps {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "transcript timing must preserve the original source-clock gaps",
            ));
        }
        validate_sha256(
            &self.content_sha256,
            "transcript.content_sha256",
            VideoErrorCode::InvalidTranscript,
        )?;
        validate_timestamp_text(&self.created_at, "transcript.created_at")?;
        validate_timed_text_items(
            self.words
                .iter()
                .map(|word| (&word.id, word.range, &word.text)),
            self.source_clock_duration_us,
            "transcript.words",
            VideoErrorCode::InvalidTranscript,
        )?;
        validate_timed_text_items(
            self.segments
                .iter()
                .map(|segment| (&segment.id, segment.range, &segment.text)),
            self.source_clock_duration_us,
            "transcript.segments",
            VideoErrorCode::InvalidTranscript,
        )?;
        let word_ids: BTreeSet<&str> = self.words.iter().map(|word| word.id.as_str()).collect();
        for segment in &self.segments {
            for word_id in &segment.word_ids {
                if !word_ids.contains(word_id.as_str()) {
                    return Err(VideoError::new(
                        VideoErrorCode::MissingReference,
                        format!("segment references missing word {word_id}"),
                    )
                    .at("transcript.segments.word_ids"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Proposed,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClipCandidate {
    pub id: String,
    pub source_asset_id: String,
    pub source_range: TimeRange,
    pub title: String,
    pub rationale: String,
    pub transcript_segment_ids: Vec<String>,
    pub score_milli: u16,
    pub status: CandidateStatus,
}

impl Validate for ClipCandidate {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "candidates.id")?;
        validate_identifier(&self.source_asset_id, "candidates.source_asset_id")?;
        self.source_range.validate()?;
        validate_nonempty(&self.title, "candidates.title", 512)?;
        validate_nonempty(&self.rationale, "candidates.rationale", 4_096)?;
        if self.score_milli > 1_000 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCandidate,
                "candidate score_milli must be in 0..=1000",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    NeedsReview,
    Approved,
    ChangesRequested,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedScene {
    pub id: String,
    pub candidate_id: Option<String>,
    pub source_asset_id: Option<String>,
    pub source_range: Option<TimeRange>,
    pub timeline_start_us: Microseconds,
    pub timeline_duration_us: Microseconds,
    pub title: String,
    pub script: String,
    pub review_state: ReviewState,
    pub revision: u32,
}

impl Validate for ReviewedScene {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "reviewed_scenes.id")?;
        if self.timeline_start_us.0 < 0 || self.timeline_duration_us.0 <= 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidScene,
                "scene timeline start must be non-negative and duration positive",
            ));
        }
        match (&self.source_asset_id, self.source_range) {
            (Some(id), Some(range)) => {
                validate_identifier(id, "reviewed_scenes.source_asset_id")?;
                range.validate()?;
            }
            (None, None) => {}
            _ => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidScene,
                    "source_asset_id and source_range must be supplied together",
                ))
            }
        }
        if let Some(candidate_id) = &self.candidate_id {
            validate_identifier(candidate_id, "reviewed_scenes.candidate_id")?;
        }
        validate_nonempty(&self.title, "reviewed_scenes.title", 512)?;
        if self.script.len() > 100_000 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidScene,
                "scene script is too large",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Overlay,
    Caption,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaReference {
    pub source_asset_id: Option<String>,
    pub render_artifact_id: Option<String>,
}

impl Validate for MediaReference {
    fn validate(&self) -> VideoResult<()> {
        match (&self.source_asset_id, &self.render_artifact_id) {
            (Some(source), None) => validate_identifier(source, "media.source_asset_id"),
            (None, Some(artifact)) => validate_identifier(artifact, "media.render_artifact_id"),
            _ => Err(VideoError::new(
                VideoErrorCode::InvalidTrack,
                "media reference must select exactly one source asset or render artifact",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRect {
    pub x_bp: i32,
    pub y_bp: i32,
    pub width_bp: i32,
    pub height_bp: i32,
}

impl Validate for NormalizedRect {
    fn validate(&self) -> VideoResult<()> {
        let right = self.x_bp.checked_add(self.width_bp);
        let bottom = self.y_bp.checked_add(self.height_bp);
        if self.x_bp < 0
            || self.y_bp < 0
            || self.width_bp <= 0
            || self.height_bp <= 0
            || right.is_none_or(|value| value > NORMALIZED_BASIS_POINTS)
            || bottom.is_none_or(|value| value > NORMALIZED_BASIS_POINTS)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "normalized rectangle must fit within 0..=10000 basis points",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineClip {
    pub id: String,
    pub scene_id: Option<String>,
    pub media: MediaReference,
    pub source_range: TimeRange,
    pub timeline_start_us: Microseconds,
    pub timeline_duration_us: Microseconds,
    pub playback_rate: RationalRate,
    pub gain_db_milli: i32,
    pub muted: bool,
    pub crop: Option<NormalizedRect>,
}

impl TimelineClip {
    pub fn timeline_range(&self) -> VideoResult<TimeRange> {
        let end = self
            .timeline_start_us
            .checked_add(self.timeline_duration_us)?;
        TimeRange::new(self.timeline_start_us.0, end.0)
    }
}

impl Validate for TimelineClip {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "tracks.clips.id")?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "tracks.clips.scene_id")?;
        }
        self.media.validate()?;
        self.source_range.validate()?;
        self.playback_rate.validate()?;
        if self.timeline_start_us.0 < 0 || self.timeline_duration_us.0 <= 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTrack,
                "clip timeline start must be non-negative and duration positive",
            ));
        }
        let source_duration = self.source_range.duration()?.0 as i128;
        let expected_left = source_duration
            .checked_mul(self.playback_rate.denominator as i128)
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::ArithmeticOverflow,
                    "clip duration validation overflowed",
                )
            })?;
        let actual_right = (self.timeline_duration_us.0 as i128)
            .checked_mul(self.playback_rate.numerator as i128)
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::ArithmeticOverflow,
                    "clip duration validation overflowed",
                )
            })?;
        if expected_left != actual_right {
            return Err(VideoError::new(
                VideoErrorCode::DurationMismatch,
                "source and timeline durations do not exactly match playback_rate",
            ));
        }
        if !(-96_000..=24_000).contains(&self.gain_db_milli) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTrack,
                "clip gain_db_milli is outside -96000..=24000",
            ));
        }
        if let Some(crop) = self.crop {
            crop.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineTrack {
    pub id: String,
    pub kind: TrackKind,
    pub clips: Vec<TimelineClip>,
    /// When true every hole must be represented by a matching TimelineGap.
    pub preserve_gaps: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    SourceSilence,
    Editorial,
    Transition,
    Padding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineGap {
    pub id: String,
    pub track_id: String,
    pub range: TimeRange,
    pub reason: GapReason,
    pub source_asset_id: Option<String>,
    pub source_range: Option<TimeRange>,
}

impl Validate for TimelineGap {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "gaps.id")?;
        validate_identifier(&self.track_id, "gaps.track_id")?;
        self.range.validate()?;
        match (&self.source_asset_id, self.source_range) {
            (Some(source_id), Some(source_range)) => {
                validate_identifier(source_id, "gaps.source_asset_id")?;
                source_range.validate()?;
                if source_range.duration()? != self.range.duration()? {
                    return Err(VideoError::new(
                        VideoErrorCode::DurationMismatch,
                        "a source-clock gap must retain its original duration",
                    ));
                }
            }
            (None, None) => {}
            _ => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidGap,
                    "source_asset_id and source_range must be supplied together",
                ))
            }
        }
        if matches!(self.reason, GapReason::SourceSilence) && self.source_range.is_none() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidGap,
                "source-silence gaps require a source-clock range",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionCue {
    pub id: String,
    pub range: TimeRange,
    pub text: String,
    pub style_id: String,
    pub speaker_id: Option<String>,
    pub transcript_segment_id: Option<String>,
    pub scene_id: Option<String>,
}

/// Stable, curated caption looks shared by manifests, native commands, agent tools, and the
/// FFmpeg/ASS renderer. Public surfaces use [`Self::public_id`]; persisted cues use
/// [`Self::manifest_id`]. Parsing accepts both forms so existing projects remain readable while
/// unsupported identifiers fail closed instead of silently changing appearance.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CaptionPresetId {
    CleanWhite,
    Calm,
    Kinetic,
    BoldPop,
    Highlight,
    Karaoke,
    Typewriter,
    Podcast,
}

impl CaptionPresetId {
    pub const ALL: [Self; 8] = [
        Self::CleanWhite,
        Self::Calm,
        Self::Kinetic,
        Self::BoldPop,
        Self::Highlight,
        Self::Karaoke,
        Self::Typewriter,
        Self::Podcast,
    ];

    pub const PUBLIC_IDS: [&'static str; 8] = [
        "clean-white",
        "calm",
        "kinetic",
        "bold-pop",
        "highlight",
        "karaoke",
        "typewriter",
        "podcast",
    ];

    pub fn parse(value: &str) -> VideoResult<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            // `clean` and unprefixed values were accepted before the curated catalog existed.
            "clean" | "clean-white" | "caption-clean" | "caption-clean-white" => {
                Ok(Self::CleanWhite)
            }
            "calm" | "caption-calm" => Ok(Self::Calm),
            "kinetic" | "caption-kinetic" => Ok(Self::Kinetic),
            "bold-pop" | "caption-bold-pop" => Ok(Self::BoldPop),
            "highlight" | "caption-highlight" => Ok(Self::Highlight),
            "karaoke" | "caption-karaoke" => Ok(Self::Karaoke),
            "typewriter" | "caption-typewriter" => Ok(Self::Typewriter),
            "podcast" | "caption-podcast" => Ok(Self::Podcast),
            _ => Err(VideoError::new(
                VideoErrorCode::InvalidCaption,
                format!(
                    "unsupported caption style; expected one of {}",
                    Self::PUBLIC_IDS.join(", ")
                ),
            )
            .at("captions.style_id")),
        }
    }

    pub const fn public_id(self) -> &'static str {
        match self {
            Self::CleanWhite => "clean-white",
            Self::Calm => "calm",
            Self::Kinetic => "kinetic",
            Self::BoldPop => "bold-pop",
            Self::Highlight => "highlight",
            Self::Karaoke => "karaoke",
            Self::Typewriter => "typewriter",
            Self::Podcast => "podcast",
        }
    }

    pub const fn manifest_id(self) -> &'static str {
        match self {
            Self::CleanWhite => "caption-clean-white",
            Self::Calm => "caption-calm",
            Self::Kinetic => "caption-kinetic",
            Self::BoldPop => "caption-bold-pop",
            Self::Highlight => "caption-highlight",
            Self::Karaoke => "caption-karaoke",
            Self::Typewriter => "caption-typewriter",
            Self::Podcast => "caption-podcast",
        }
    }
}

impl Validate for CaptionCue {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "captions.id")?;
        self.range.validate()?;
        validate_nonempty(&self.text, "captions.text", 2_000)?;
        CaptionPresetId::parse(&self.style_id)?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "captions.scene_id")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasMode {
    Portrait,
    Landscape,
    Square,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasSpec {
    pub width: u32,
    pub height: u32,
    pub pixel_aspect_numerator: u32,
    pub pixel_aspect_denominator: u32,
}

impl Validate for CanvasSpec {
    fn validate(&self) -> VideoResult<()> {
        if self.width == 0
            || self.height == 0
            || self.width > 16_384
            || self.height > 16_384
            || self.pixel_aspect_numerator == 0
            || self.pixel_aspect_denominator == 0
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "canvas dimensions/aspect are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutRole {
    PrimaryVideo,
    SecondaryVideo,
    SpeakerCard,
    TitleCard,
    Captions,
    Waveform,
    KineticText,
    Artwork,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutElement {
    pub id: String,
    pub role: LayoutRole,
    pub scene_id: Option<String>,
    pub bounds: NormalizedRect,
    pub z_index: i16,
    pub style_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutPlan {
    pub mode: CanvasMode,
    pub canvas: CanvasSpec,
    pub safe_area: NormalizedRect,
    pub background_rgba: [u8; 4],
    pub elements: Vec<LayoutElement>,
}

impl Validate for LayoutPlan {
    fn validate(&self) -> VideoResult<()> {
        self.canvas.validate()?;
        self.safe_area.validate()?;
        validate_unique_ids(
            self.elements.iter().map(|element| element.id.as_str()),
            "layout.elements",
        )?;
        for element in &self.elements {
            validate_identifier(&element.id, "layout.elements.id")?;
            element.bounds.validate()?;
            if let Some(scene_id) = &element.scene_id {
                validate_identifier(scene_id, "layout.elements.scene_id")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DuckingSpec {
    pub sidechain_track_id: String,
    pub reduction_db_milli: i32,
    pub attack_us: Microseconds,
    pub release_us: Microseconds,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioMixTrack {
    pub track_id: String,
    pub gain_db_milli: i32,
    pub pan_milli: i16,
    pub ducking: Option<DuckingSpec>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioMix {
    pub target_lufs_milli: i32,
    pub true_peak_db_milli: i32,
    pub tracks: Vec<AudioMixTrack>,
}

impl Validate for AudioMix {
    fn validate(&self) -> VideoResult<()> {
        if !(-36_000..=-5_000).contains(&self.target_lufs_milli)
            || !(-12_000..=0).contains(&self.true_peak_db_milli)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAudioMix,
                "loudness targets are outside safe supported bounds",
            ));
        }
        validate_unique_ids(
            self.tracks.iter().map(|track| track.track_id.as_str()),
            "audio_mix.tracks",
        )?;
        for track in &self.tracks {
            validate_identifier(&track.track_id, "audio_mix.tracks.track_id")?;
            if !(-96_000..=24_000).contains(&track.gain_db_milli)
                || !(-1_000..=1_000).contains(&track.pan_milli)
            {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAudioMix,
                    "audio track gain or pan is outside supported bounds",
                ));
            }
            if let Some(ducking) = &track.ducking {
                validate_identifier(
                    &ducking.sidechain_track_id,
                    "audio_mix.tracks.ducking.sidechain_track_id",
                )?;
                if !(-60_000..=0).contains(&ducking.reduction_db_milli)
                    || ducking.attack_us.0 < 0
                    || ducking.release_us.0 < 0
                {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidAudioMix,
                        "ducking parameters are invalid",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderArtifactRole {
    Proxy,
    Thumbnail,
    Waveform,
    SceneSegment,
    Preview,
    FinalMaster,
    Captions,
    Transcript,
    PublishPackage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    Staged,
    Published,
    Superseded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenderArtifact {
    pub id: String,
    pub role: RenderArtifactRole,
    pub scene_id: Option<String>,
    pub managed_path: String,
    pub sha256: String,
    pub cache_key: String,
    pub mime_type: String,
    pub duration_us: Option<Microseconds>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub publication_state: PublicationState,
    pub created_at: String,
}

impl Validate for RenderArtifact {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "render_artifacts.id")?;
        validate_managed_path(&self.managed_path, "render_artifacts.managed_path")?;
        validate_sha256(
            &self.sha256,
            "render_artifacts.sha256",
            VideoErrorCode::InvalidArtifact,
        )?;
        validate_sha256(
            &self.cache_key,
            "render_artifacts.cache_key",
            VideoErrorCode::InvalidArtifact,
        )?;
        validate_nonempty(&self.mime_type, "render_artifacts.mime_type", 256)?;
        validate_timestamp_text(&self.created_at, "render_artifacts.created_at")?;
        if self.duration_us.is_some_and(|duration| duration.0 <= 0)
            || self.width.is_some_and(|width| width == 0)
            || self.height.is_some_and(|height| height == 0)
            || self.width.is_some() != self.height.is_some()
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidArtifact,
                "artifact media dimensions or duration are invalid",
            ));
        }
        Ok(())
    }
}

/// Binds the active generated narration for a scene to both its soundAr History
/// provenance and the immutable managed artifact used by the canonical timeline.
///
/// The generation route is stored explicitly so a later voice revision can be
/// reproduced without guessing which model, speaker, or consent-backed voice was
/// used. `script_sha256` prevents a narration take from silently surviving a
/// script edit that changed the words it is meant to speak.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NarrationBinding {
    pub id: String,
    pub scene_id: Option<String>,
    pub render_artifact_id: String,
    pub history_id: String,
    pub generation_job_id: String,
    pub voice_id: String,
    pub model_id: String,
    pub speaker: String,
    pub language: String,
    pub script_sha256: String,
    pub created_at: String,
}

impl Validate for NarrationBinding {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "narration_bindings.id")?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "narration_bindings.scene_id")?;
        }
        validate_identifier(
            &self.render_artifact_id,
            "narration_bindings.render_artifact_id",
        )?;
        validate_identifier(&self.history_id, "narration_bindings.history_id")?;
        validate_identifier(
            &self.generation_job_id,
            "narration_bindings.generation_job_id",
        )?;
        validate_identifier(&self.voice_id, "narration_bindings.voice_id")?;
        validate_nonempty(&self.model_id, "narration_bindings.model_id", 256)?;
        validate_nonempty(&self.speaker, "narration_bindings.speaker", 128)?;
        validate_language_tag(&self.language, "narration_bindings.language")?;
        validate_sha256(
            &self.script_sha256,
            "narration_bindings.script_sha256",
            VideoErrorCode::InvalidNarration,
        )?;
        validate_timestamp_text(&self.created_at, "narration_bindings.created_at")?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionStage {
    Ingest,
    Transcript,
    Analysis,
    Plan,
    Speech,
    Music,
    Captions,
    Tracking,
    SceneRender,
    Preview,
    FinalRender,
    PublishPackage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionRecord {
    pub id: String,
    pub revision: u64,
    pub parent_id: Option<String>,
    pub actor: String,
    pub reason: String,
    pub changed_paths: Vec<String>,
    pub invalidated_stages: BTreeSet<RevisionStage>,
    pub created_at: String,
}

impl Validate for RevisionRecord {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "revision_history.id")?;
        if let Some(parent_id) = &self.parent_id {
            validate_identifier(parent_id, "revision_history.parent_id")?;
        }
        validate_nonempty(&self.actor, "revision_history.actor", 256)?;
        validate_nonempty(&self.reason, "revision_history.reason", 4_096)?;
        validate_timestamp_text(&self.created_at, "revision_history.created_at")?;
        if self.changed_paths.is_empty()
            || self
                .changed_paths
                .iter()
                .any(|path| !path.starts_with('/') || path.len() > 1_024)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidRevision,
                "revision changed_paths must contain JSON-pointer-style paths",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProjectManifest {
    pub schema_version: u32,
    pub project_id: String,
    pub name: String,
    pub revision: u64,
    pub frame_rate: RationalFrameRate,
    pub timeline_duration_us: Microseconds,
    pub source_assets: Vec<SourceAsset>,
    pub rights_confirmations: Vec<RightsConfirmation>,
    pub transcript: Option<TranscriptVersion>,
    pub candidates: Vec<ClipCandidate>,
    pub reviewed_scenes: Vec<ReviewedScene>,
    pub tracks: Vec<TimelineTrack>,
    pub gaps: Vec<TimelineGap>,
    pub captions: Vec<CaptionCue>,
    pub layout: LayoutPlan,
    pub audio_mix: AudioMix,
    #[serde(default)]
    pub narration_bindings: Vec<NarrationBinding>,
    pub render_artifacts: Vec<RenderArtifact>,
    pub revision_history: Vec<RevisionRecord>,
    pub created_at: String,
    pub updated_at: String,
}

impl VideoProjectManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: impl Into<String>,
        name: impl Into<String>,
        frame_rate: RationalFrameRate,
        timeline_duration_us: Microseconds,
        layout: LayoutPlan,
        audio_mix: AudioMix,
        created_at: impl Into<String>,
    ) -> VideoResult<Self> {
        let created_at = created_at.into();
        let manifest = Self {
            schema_version: VIDEO_MANIFEST_SCHEMA_VERSION,
            project_id: project_id.into(),
            name: name.into(),
            revision: 0,
            frame_rate,
            timeline_duration_us,
            source_assets: Vec::new(),
            rights_confirmations: Vec::new(),
            transcript: None,
            candidates: Vec::new(),
            reviewed_scenes: Vec::new(),
            tracks: Vec::new(),
            gaps: Vec::new(),
            captions: Vec::new(),
            layout,
            audio_mix,
            narration_bindings: Vec::new(),
            render_artifacts: Vec::new(),
            revision_history: Vec::new(),
            created_at: created_at.clone(),
            updated_at: created_at,
        };
        manifest.validate_strict()?;
        Ok(manifest)
    }

    pub fn validate_strict(&self) -> VideoResult<()> {
        self.validate()
    }
}

impl Validate for VideoProjectManifest {
    fn validate(&self) -> VideoResult<()> {
        if self.schema_version != VIDEO_MANIFEST_SCHEMA_VERSION {
            return Err(VideoError::new(
                VideoErrorCode::UnsupportedSchema,
                format!(
                    "manifest schema {} is unsupported; expected {}",
                    self.schema_version, VIDEO_MANIFEST_SCHEMA_VERSION
                ),
            )
            .at("schema_version"));
        }
        validate_identifier(&self.project_id, "project_id")?;
        validate_nonempty(&self.name, "name", 512)?;
        self.frame_rate.validate()?;
        if !(1..=MAX_TIMELINE_DURATION_US).contains(&self.timeline_duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTimestamp,
                "timeline_duration_us must be positive and no greater than 6 hours",
            )
            .at("timeline_duration_us"));
        }
        validate_timestamp_text(&self.created_at, "created_at")?;
        validate_timestamp_text(&self.updated_at, "updated_at")?;

        validate_unique_ids(
            self.source_assets.iter().map(|asset| asset.id.as_str()),
            "source_assets",
        )?;
        validate_unique_ids(
            self.rights_confirmations
                .iter()
                .map(|rights| rights.id.as_str()),
            "rights_confirmations",
        )?;
        validate_unique_ids(
            self.candidates
                .iter()
                .map(|candidate| candidate.id.as_str()),
            "candidates",
        )?;
        validate_unique_ids(
            self.reviewed_scenes.iter().map(|scene| scene.id.as_str()),
            "reviewed_scenes",
        )?;
        validate_unique_ids(self.tracks.iter().map(|track| track.id.as_str()), "tracks")?;
        validate_unique_ids(self.gaps.iter().map(|gap| gap.id.as_str()), "gaps")?;
        validate_unique_ids(
            self.captions.iter().map(|caption| caption.id.as_str()),
            "captions",
        )?;
        validate_unique_ids(
            self.render_artifacts
                .iter()
                .map(|artifact| artifact.id.as_str()),
            "render_artifacts",
        )?;
        validate_unique_ids(
            self.narration_bindings
                .iter()
                .map(|binding| binding.id.as_str()),
            "narration_bindings",
        )?;
        validate_unique_ids(
            self.revision_history
                .iter()
                .map(|revision| revision.id.as_str()),
            "revision_history",
        )?;

        let source_by_id: BTreeMap<&str, &SourceAsset> = self
            .source_assets
            .iter()
            .map(|asset| (asset.id.as_str(), asset))
            .collect();
        let rights_by_id: BTreeMap<&str, &RightsConfirmation> = self
            .rights_confirmations
            .iter()
            .map(|rights| (rights.id.as_str(), rights))
            .collect();
        let artifact_by_id: BTreeMap<&str, &RenderArtifact> = self
            .render_artifacts
            .iter()
            .map(|artifact| (artifact.id.as_str(), artifact))
            .collect();
        let candidate_ids: BTreeSet<&str> = self
            .candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect();
        let scene_by_id: BTreeMap<&str, &ReviewedScene> = self
            .reviewed_scenes
            .iter()
            .map(|scene| (scene.id.as_str(), scene))
            .collect();
        let track_by_id: BTreeMap<&str, &TimelineTrack> = self
            .tracks
            .iter()
            .map(|track| (track.id.as_str(), track))
            .collect();
        let track_ids: BTreeSet<&str> = track_by_id.keys().copied().collect();

        for rights in &self.rights_confirmations {
            rights.validate()?;
        }
        for asset in &self.source_assets {
            asset.validate()?;
            if let Some(rights_id) = &asset.rights_confirmation_id {
                let rights = rights_by_id.get(rights_id.as_str()).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        format!("asset {} references missing rights record", asset.id),
                    )
                })?;
                if matches!(asset.kind, SourceAssetKind::ImportedLink)
                    && asset.provenance.original_uri.as_deref() != Some(rights.source_uri.as_str())
                {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidRightsConfirmation,
                        "rights record is not bound to the imported source URI",
                    )
                    .at("source_assets.rights_confirmation_id"));
                }
            }
        }

        let transcript_segment_ids = if let Some(transcript) = &self.transcript {
            transcript.validate()?;
            let source = source_by_id
                .get(transcript.source_asset_id.as_str())
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "transcript references a missing source asset",
                    )
                })?;
            if transcript.source_clock_duration_us != source.probe.duration_us {
                return Err(VideoError::new(
                    VideoErrorCode::DurationMismatch,
                    "transcript source clock must equal probed source duration",
                ));
            }
            transcript
                .segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };

        for candidate in &self.candidates {
            candidate.validate()?;
            let source = source_by_id
                .get(candidate.source_asset_id.as_str())
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        format!("candidate {} references missing source", candidate.id),
                    )
                })?;
            validate_range_within(
                candidate.source_range,
                source.probe.duration_us,
                VideoErrorCode::InvalidCandidate,
                "candidates.source_range",
            )?;
            if candidate
                .transcript_segment_ids
                .iter()
                .any(|id| !transcript_segment_ids.contains(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "candidate references a missing transcript segment",
                ));
            }
        }

        for scene in &self.reviewed_scenes {
            scene.validate()?;
            if scene
                .candidate_id
                .as_ref()
                .is_some_and(|id| !candidate_ids.contains(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    format!("scene {} references missing candidate", scene.id),
                ));
            }
            if let (Some(source_id), Some(range)) = (&scene.source_asset_id, scene.source_range) {
                let source = source_by_id.get(source_id.as_str()).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        format!("scene {} references missing source", scene.id),
                    )
                })?;
                validate_range_within(
                    range,
                    source.probe.duration_us,
                    VideoErrorCode::InvalidScene,
                    "reviewed_scenes.source_range",
                )?;
            }
            let scene_end = scene
                .timeline_start_us
                .checked_add(scene.timeline_duration_us)?;
            if scene_end > self.timeline_duration_us {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidScene,
                    "scene extends beyond the manifest timeline",
                ));
            }
        }

        for artifact in &self.render_artifacts {
            artifact.validate()?;
            if artifact
                .scene_id
                .as_ref()
                .is_some_and(|id| !scene_by_id.contains_key(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "render artifact references a missing scene",
                ));
            }
        }

        let mut bound_scene_ids = BTreeSet::new();
        for binding in &self.narration_bindings {
            binding.validate()?;
            if let Some(scene_id) = binding.scene_id.as_deref() {
                let scene = scene_by_id.get(scene_id).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "narration binding references a missing scene",
                    )
                    .at("narration_bindings.scene_id")
                })?;
                if !bound_scene_ids.insert(scene_id) {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidNarration,
                        "a scene may have only one active narration binding",
                    )
                    .at("narration_bindings.scene_id"));
                }
                let expected_script_sha256 =
                    format!("{:x}", Sha256::digest(scene.script.as_bytes()));
                if binding.script_sha256 != expected_script_sha256 {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidNarration,
                        "narration script provenance does not match the current scene script",
                    )
                    .at("narration_bindings.script_sha256"));
                }
            }
            let artifact = artifact_by_id
                .get(binding.render_artifact_id.as_str())
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "narration binding references a missing render artifact",
                    )
                    .at("narration_bindings.render_artifact_id")
                })?;
            if !artifact.mime_type.starts_with("audio/")
                || artifact.duration_us.is_none()
                || !matches!(artifact.publication_state, PublicationState::Published)
            {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidNarration,
                    "narration must reference a published audio artifact with a duration",
                )
                .at("narration_bindings.render_artifact_id"));
            }
        }

        let mut clip_ids = BTreeSet::new();
        for track in &self.tracks {
            validate_identifier(&track.id, "tracks.id")?;
            let mut clip_ranges = Vec::with_capacity(track.clips.len());
            for clip in &track.clips {
                clip.validate()?;
                if !clip_ids.insert(clip.id.as_str()) {
                    return Err(duplicate_id_error("tracks.clips", &clip.id));
                }
                if clip
                    .scene_id
                    .as_ref()
                    .is_some_and(|id| !scene_by_id.contains_key(id.as_str()))
                {
                    return Err(VideoError::new(
                        VideoErrorCode::MissingReference,
                        "timeline clip references a missing scene",
                    ));
                }
                if let Some(scene_id) = &clip.scene_id {
                    if matches!(
                        scene_by_id[scene_id.as_str()].review_state,
                        ReviewState::Rejected
                    ) {
                        return Err(VideoError::new(
                            VideoErrorCode::InvalidScene,
                            "a rejected scene cannot be placed on the timeline",
                        ));
                    }
                }
                match (&clip.media.source_asset_id, &clip.media.render_artifact_id) {
                    (Some(source_id), None) => {
                        let source = source_by_id.get(source_id.as_str()).ok_or_else(|| {
                            VideoError::new(
                                VideoErrorCode::MissingReference,
                                "timeline clip references a missing source asset",
                            )
                        })?;
                        validate_range_within(
                            clip.source_range,
                            source.probe.duration_us,
                            VideoErrorCode::InvalidTrack,
                            "tracks.clips.source_range",
                        )?;
                        validate_source_stream_for_track(track, source)?;
                    }
                    (None, Some(artifact_id)) => {
                        let artifact =
                            artifact_by_id.get(artifact_id.as_str()).ok_or_else(|| {
                                VideoError::new(
                                    VideoErrorCode::MissingReference,
                                    "timeline clip references a missing render artifact",
                                )
                            })?;
                        let duration = artifact.duration_us.ok_or_else(|| {
                            VideoError::new(
                                VideoErrorCode::InvalidArtifact,
                                "timeline media artifacts must declare their source-clock duration",
                            )
                            .at("render_artifacts.duration_us")
                        })?;
                        if !matches!(artifact.publication_state, PublicationState::Published) {
                            return Err(VideoError::new(
                                VideoErrorCode::InvalidArtifact,
                                "timeline clips may reference only atomically published artifacts",
                            )
                            .at("render_artifacts.publication_state"));
                        }
                        validate_range_within(
                            clip.source_range,
                            duration,
                            VideoErrorCode::InvalidTrack,
                            "tracks.clips.source_range",
                        )?;
                        validate_artifact_stream_for_track(track, artifact)?;
                    }
                    _ => {
                        return Err(VideoError::new(
                            VideoErrorCode::MissingReference,
                            "timeline clip references a missing render artifact",
                        ))
                    }
                }
                let range = clip.timeline_range()?;
                validate_range_within(
                    range,
                    self.timeline_duration_us,
                    VideoErrorCode::InvalidTrack,
                    "tracks.clips.timeline",
                )?;
                clip_ranges.push(range);
            }
            validate_non_overlapping_ranges(clip_ranges, "tracks.clips")?;
        }

        for gap in &self.gaps {
            gap.validate()?;
            if !track_ids.contains(gap.track_id.as_str()) {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    format!("gap {} references a missing track", gap.id),
                ));
            }
            validate_range_within(
                gap.range,
                self.timeline_duration_us,
                VideoErrorCode::InvalidGap,
                "gaps.range",
            )?;
            if let (Some(source_id), Some(source_range)) = (&gap.source_asset_id, gap.source_range)
            {
                let source = source_by_id.get(source_id.as_str()).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "gap references a missing source asset",
                    )
                })?;
                validate_range_within(
                    source_range,
                    source.probe.duration_us,
                    VideoErrorCode::InvalidGap,
                    "gaps.source_range",
                )?;
            }
        }
        for track in self.tracks.iter().filter(|track| track.preserve_gaps) {
            validate_track_partition(track, &self.gaps, self.timeline_duration_us)?;
        }

        let mut caption_ranges = Vec::with_capacity(self.captions.len());
        for caption in &self.captions {
            caption.validate()?;
            validate_range_within(
                caption.range,
                self.timeline_duration_us,
                VideoErrorCode::InvalidCaption,
                "captions.range",
            )?;
            if caption
                .transcript_segment_id
                .as_ref()
                .is_some_and(|id| !transcript_segment_ids.contains(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "caption references a missing transcript segment",
                ));
            }
            if caption
                .scene_id
                .as_ref()
                .is_some_and(|id| !scene_by_id.contains_key(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "caption references a missing scene",
                ));
            }
            caption_ranges.push(caption.range);
        }
        validate_non_overlapping_ranges(caption_ranges, "captions")?;

        self.layout.validate()?;
        for element in &self.layout.elements {
            if element
                .scene_id
                .as_ref()
                .is_some_and(|id| !scene_by_id.contains_key(id.as_str()))
            {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "layout element references a missing scene",
                ));
            }
        }
        self.audio_mix.validate()?;
        for mix_track in &self.audio_mix.tracks {
            let track = track_by_id
                .get(mix_track.track_id.as_str())
                .ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::MissingReference,
                        "audio mix references a missing timeline track",
                    )
                })?;
            if !matches!(track.kind, TrackKind::Audio) {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAudioMix,
                    "audio mix entries may reference only audio tracks",
                )
                .at("audio_mix.tracks.track_id"));
            }
            if let Some(ducking) = &mix_track.ducking {
                let sidechain = track_by_id
                    .get(ducking.sidechain_track_id.as_str())
                    .ok_or_else(|| {
                        VideoError::new(
                            VideoErrorCode::MissingReference,
                            "audio ducking references a missing sidechain track",
                        )
                    })?;
                if !matches!(sidechain.kind, TrackKind::Audio) || sidechain.id == mix_track.track_id
                {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidAudioMix,
                        "audio ducking requires a different audio sidechain track",
                    )
                    .at("audio_mix.tracks.ducking.sidechain_track_id"));
                }
            }
        }

        validate_revision_chain(&self.revision_history, self.revision)?;
        Ok(())
    }
}

fn validate_source_stream_for_track(
    track: &TimelineTrack,
    source: &SourceAsset,
) -> VideoResult<()> {
    let valid = match track.kind {
        TrackKind::Video | TrackKind::Overlay => source.probe.has_video,
        TrackKind::Audio => source.probe.has_audio,
        TrackKind::Caption => false,
    };
    if valid {
        Ok(())
    } else {
        Err(VideoError::new(
            VideoErrorCode::InvalidTrack,
            "timeline track kind is incompatible with the referenced source streams",
        )
        .at("tracks.clips.media.source_asset_id"))
    }
}

fn validate_artifact_stream_for_track(
    track: &TimelineTrack,
    artifact: &RenderArtifact,
) -> VideoResult<()> {
    let mime = artifact.mime_type.to_ascii_lowercase();
    let valid = match track.kind {
        TrackKind::Video | TrackKind::Overlay => {
            mime.starts_with("video/") && artifact.width.is_some() && artifact.height.is_some()
        }
        // A rendered video segment may legitimately provide an audio pad. The
        // manifest lacks per-stream artifact probes, so video and audio MIME
        // types are the strongest compatible contract available here.
        TrackKind::Audio => mime.starts_with("audio/") || mime.starts_with("video/"),
        TrackKind::Caption => {
            mime.starts_with("text/")
                || matches!(
                    mime.as_str(),
                    "application/x-ass" | "application/x-subrip" | "application/ttml+xml"
                )
        }
    };
    if valid {
        Ok(())
    } else {
        Err(VideoError::new(
            VideoErrorCode::InvalidTrack,
            "timeline track kind is incompatible with the referenced artifact",
        )
        .at("tracks.clips.media.render_artifact_id"))
    }
}

pub(crate) fn validate_identifier(value: &str, field: &str) -> VideoResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid {
        return Err(VideoError::new(
            VideoErrorCode::InvalidIdentifier,
            "identifier must be 1..=128 ASCII letters, digits, '-', '_', '.', or ':'",
        )
        .at(field));
    }
    Ok(())
}

pub(crate) fn validate_sha256(value: &str, field: &str, code: VideoErrorCode) -> VideoResult<()> {
    if value.len() != SHA256_HEX_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(VideoError::new(
            code,
            "SHA-256 values must be 64 lowercase hexadecimal characters",
        )
        .at(field));
    }
    Ok(())
}

fn validate_nonempty(value: &str, field: &str, maximum_length: usize) -> VideoResult<()> {
    if value.trim().is_empty() || value.len() > maximum_length {
        return Err(VideoError::new(
            VideoErrorCode::InvalidIdentifier,
            format!("value must be non-empty and at most {maximum_length} bytes"),
        )
        .at(field));
    }
    Ok(())
}

fn validate_language_tag(value: &str, field: &str) -> VideoResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.split('-').all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    if !valid {
        return Err(VideoError::new(
            VideoErrorCode::InvalidNarration,
            "language must be a bounded BCP-47-style tag",
        )
        .at(field));
    }
    Ok(())
}

fn validate_timestamp_text(value: &str, field: &str) -> VideoResult<()> {
    if !value.ends_with('Z') || DateTime::parse_from_rfc3339(value).is_err() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTimestamp,
            "timestamp must be a UTC RFC3339 string",
        )
        .at(field));
    }
    Ok(())
}

const fn gcd_u32(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn validate_managed_path(value: &str, field: &str) -> VideoResult<()> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > 4_096
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(VideoError::new(
            VideoErrorCode::InvalidAsset,
            "managed paths must be relative, bounded, and may not traverse parents",
        )
        .at(field));
    }
    Ok(())
}

fn validate_unique_ids<'a>(ids: impl IntoIterator<Item = &'a str>, field: &str) -> VideoResult<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(duplicate_id_error(field, id));
        }
    }
    Ok(())
}

fn duplicate_id_error(field: &str, id: &str) -> VideoError {
    VideoError::new(
        VideoErrorCode::DuplicateId,
        format!("duplicate identifier {id}"),
    )
    .at(field)
}

fn validate_range_within(
    range: TimeRange,
    duration: Microseconds,
    code: VideoErrorCode,
    field: &str,
) -> VideoResult<()> {
    range.validate()?;
    if duration.0 <= 0 || range.end_us > duration {
        return Err(VideoError::new(code, "time range exceeds its clock duration").at(field));
    }
    Ok(())
}

fn validate_timed_text_items<'a>(
    items: impl IntoIterator<Item = (&'a String, TimeRange, &'a String)>,
    duration: Microseconds,
    field: &str,
    code: VideoErrorCode,
) -> VideoResult<()> {
    let mut seen = BTreeSet::new();
    let mut previous_end = Microseconds::ZERO;
    for (id, range, text) in items {
        validate_identifier(id, field)?;
        if !seen.insert(id.as_str()) {
            return Err(duplicate_id_error(field, id));
        }
        validate_range_within(range, duration, code, field)?;
        if range.start_us < previous_end {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "source-clock transcript entries overlap or are not ordered",
            )
            .at(field));
        }
        if text.trim().is_empty() || text.len() > 20_000 {
            return Err(VideoError::new(code, "timed text is empty or too large").at(field));
        }
        previous_end = range.end_us;
    }
    Ok(())
}

fn validate_non_overlapping_ranges(mut ranges: Vec<TimeRange>, field: &str) -> VideoResult<()> {
    ranges.sort_by_key(|range| (range.start_us, range.end_us));
    for pair in ranges.windows(2) {
        if pair[1].start_us < pair[0].end_us {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "timeline entries overlap",
            )
            .at(field));
        }
    }
    Ok(())
}

fn validate_track_partition(
    track: &TimelineTrack,
    all_gaps: &[TimelineGap],
    duration: Microseconds,
) -> VideoResult<()> {
    let mut spans: Vec<(TimeRange, bool)> = track
        .clips
        .iter()
        .map(|clip| clip.timeline_range().map(|range| (range, false)))
        .collect::<VideoResult<_>>()?;
    spans.extend(
        all_gaps
            .iter()
            .filter(|gap| gap.track_id == track.id)
            .map(|gap| (gap.range, true)),
    );
    spans.sort_by_key(|(range, is_gap)| (range.start_us, range.end_us, *is_gap));
    if spans.is_empty()
        || spans[0].0.start_us != Microseconds::ZERO
        || spans.last().map(|span| span.0.end_us) != Some(duration)
    {
        return Err(VideoError::new(
            VideoErrorCode::TimelineGap,
            "gap-preserving tracks must explicitly cover the complete timeline",
        )
        .at("tracks.preserve_gaps"));
    }
    for pair in spans.windows(2) {
        if pair[0].0.end_us < pair[1].0.start_us {
            return Err(VideoError::new(
                VideoErrorCode::TimelineGap,
                "implicit timeline hole; add a TimelineGap",
            )
            .at("gaps"));
        }
        if pair[0].0.end_us > pair[1].0.start_us {
            return Err(VideoError::new(
                VideoErrorCode::TimelineOverlap,
                "timeline clips and gaps overlap",
            )
            .at("gaps"));
        }
    }
    Ok(())
}

fn validate_revision_chain(history: &[RevisionRecord], current_revision: u64) -> VideoResult<()> {
    if history.is_empty() {
        if current_revision == 0 {
            return Ok(());
        }
        return Err(VideoError::new(
            VideoErrorCode::InvalidRevision,
            "non-zero manifest revision requires revision history",
        ));
    }
    let mut previous_id: Option<&str> = None;
    let mut expected_revision = 1_u64;
    for record in history {
        record.validate()?;
        if record.revision != expected_revision || record.parent_id.as_deref() != previous_id {
            return Err(VideoError::new(
                VideoErrorCode::InvalidRevision,
                "revision history must be contiguous with exact parent links",
            ));
        }
        previous_id = Some(record.id.as_str());
        expected_revision = expected_revision.checked_add(1).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::ArithmeticOverflow,
                "revision number overflowed",
            )
        })?;
    }
    if history.last().map(|record| record.revision) != Some(current_revision) {
        return Err(VideoError::new(
            VideoErrorCode::InvalidRevision,
            "manifest revision does not match revision history",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn timestamp() -> String {
        "2026-08-27T12:00:00Z".to_owned()
    }

    fn source() -> SourceAsset {
        SourceAsset {
            id: "source-1".into(),
            kind: SourceAssetKind::LocalVideo,
            managed_path: "video/source-1/original.mp4".into(),
            sha256: hash('a'),
            probe: MediaProbe {
                duration_us: Microseconds(3_000_000),
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(RationalFrameRate::FPS_30000_1001),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: timestamp(),
                producer: "soundar".into(),
                producer_version: Some("0.6.0".into()),
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        }
    }

    fn clip(id: &str, source_start: i64, timeline_start: i64, duration: i64) -> TimelineClip {
        TimelineClip {
            id: id.into(),
            scene_id: None,
            media: MediaReference {
                source_asset_id: Some("source-1".into()),
                render_artifact_id: None,
            },
            source_range: TimeRange::new(source_start, source_start + duration).unwrap(),
            timeline_start_us: Microseconds(timeline_start),
            timeline_duration_us: Microseconds(duration),
            playback_rate: RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        }
    }

    fn manifest() -> VideoProjectManifest {
        VideoProjectManifest {
            schema_version: VIDEO_MANIFEST_SCHEMA_VERSION,
            project_id: "project-1".into(),
            name: "Test reel".into(),
            revision: 0,
            frame_rate: RationalFrameRate::FPS_30000_1001,
            timeline_duration_us: Microseconds(3_000_000),
            source_assets: vec![source()],
            rights_confirmations: vec![],
            transcript: None,
            candidates: vec![],
            reviewed_scenes: vec![],
            tracks: vec![TimelineTrack {
                id: "video-main".into(),
                kind: TrackKind::Video,
                clips: vec![
                    clip("clip-1", 0, 0, 1_000_000),
                    clip("clip-2", 2_000_000, 2_000_000, 1_000_000),
                ],
                preserve_gaps: true,
            }],
            gaps: vec![TimelineGap {
                id: "gap-1".into(),
                track_id: "video-main".into(),
                range: TimeRange::new(1_000_000, 2_000_000).unwrap(),
                reason: GapReason::SourceSilence,
                source_asset_id: Some("source-1".into()),
                source_range: Some(TimeRange::new(1_000_000, 2_000_000).unwrap()),
            }],
            captions: vec![],
            layout: LayoutPlan {
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
                background_rgba: [245, 245, 244, 255],
                elements: vec![],
            },
            audio_mix: AudioMix {
                target_lufs_milli: -16_000,
                true_peak_db_milli: -1_000,
                tracks: vec![],
            },
            narration_bindings: vec![],
            render_artifacts: vec![],
            revision_history: vec![],
            created_at: timestamp(),
            updated_at: timestamp(),
        }
    }

    #[test]
    fn valid_manifest_preserves_explicit_gap() {
        manifest().validate_strict().unwrap();
    }

    #[test]
    fn curated_caption_ids_are_stable_and_unknown_styles_fail_closed() {
        for preset in CaptionPresetId::ALL {
            assert_eq!(CaptionPresetId::parse(preset.public_id()).unwrap(), preset);
            assert_eq!(
                CaptionPresetId::parse(preset.manifest_id()).unwrap(),
                preset
            );
        }
        assert_eq!(
            CaptionPresetId::parse("clean").unwrap(),
            CaptionPresetId::CleanWhite
        );

        let mut manifest = manifest();
        manifest.captions.push(CaptionCue {
            id: "caption-unsupported".into(),
            range: TimeRange::new(0, 1_000_000).unwrap(),
            text: "A caption must never silently change style.".into(),
            style_id: "surprise-template".into(),
            speaker_id: None,
            transcript_segment_id: None,
            scene_id: None,
        });
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCaption);
        assert_eq!(error.field.as_deref(), Some("captions.style_id"));
    }

    #[test]
    fn implicit_gap_is_rejected_with_stable_code() {
        let mut manifest = manifest();
        manifest.gaps.clear();
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::TimelineGap);
        assert_eq!(error.stable_code(), "video.timeline_gap");
    }

    #[test]
    fn imported_links_require_per_url_rights() {
        let mut asset = source();
        asset.kind = SourceAssetKind::ImportedLink;
        asset.provenance.kind = ProvenanceKind::AuthorizedLink;
        asset.provenance.original_uri = Some("https://example.com/watch?v=one".into());
        let error = asset.validate().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingRightsConfirmation);
    }

    #[test]
    fn rights_confirmation_is_cryptographically_bound_to_one_exact_url() {
        let source_uri = "https://example.com/watch?v=one";
        let mut rights = RightsConfirmation {
            id: "rights-1".into(),
            source_uri: source_uri.into(),
            source_uri_sha256: format!("{:x}", Sha256::digest(source_uri.as_bytes())),
            basis: RightsBasis::Owned,
            confirmation_text: "I own this source and authorize this import.".into(),
            confirmed_by: "local-user".into(),
            confirmed_at: timestamp(),
            single_source_only: true,
        };
        rights.validate().unwrap();
        rights.source_uri = "https://example.com/watch?v=two".into();
        let error = rights.validate().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidRightsConfirmation);
    }

    #[test]
    fn new_manifest_constructs_a_strict_empty_service_document() {
        let template = manifest();
        let created = VideoProjectManifest::new(
            "project-new",
            "New video",
            RationalFrameRate::FPS_24,
            Microseconds(1_000_000),
            template.layout,
            template.audio_mix,
            timestamp(),
        )
        .unwrap();
        assert_eq!(created.schema_version, VIDEO_MANIFEST_SCHEMA_VERSION);
        assert_eq!(created.revision, 0);
        assert!(created.source_assets.is_empty());
        created.validate_strict().unwrap();
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_field".into(), Value::Bool(true));
        assert!(serde_json::from_value::<VideoProjectManifest>(value).is_err());
    }

    #[test]
    fn legacy_manifests_default_to_no_narration_bindings() {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value.as_object_mut().unwrap().remove("narration_bindings");
        let decoded = serde_json::from_value::<VideoProjectManifest>(value).unwrap();
        assert!(decoded.narration_bindings.is_empty());
        decoded.validate_strict().unwrap();
    }

    #[test]
    fn narration_binding_is_audio_backed_and_bound_to_current_scene_script() {
        let mut manifest = manifest();
        let script = "This narration belongs to the reviewed scene.";
        manifest.reviewed_scenes.push(ReviewedScene {
            id: "scene-narration".into(),
            candidate_id: None,
            source_asset_id: Some("source-1".into()),
            source_range: Some(TimeRange::new(0, 3_000_000).unwrap()),
            timeline_start_us: Microseconds::ZERO,
            timeline_duration_us: Microseconds(3_000_000),
            title: "Narrated scene".into(),
            script: script.into(),
            review_state: ReviewState::Approved,
            revision: 1,
        });
        manifest.render_artifacts.push(RenderArtifact {
            id: "narration-audio".into(),
            role: RenderArtifactRole::SceneSegment,
            scene_id: Some("scene-narration".into()),
            managed_path: "renders/narration-audio.wav".into(),
            sha256: hash('b'),
            cache_key: hash('c'),
            mime_type: "audio/wav".into(),
            duration_us: Some(Microseconds(3_000_000)),
            width: None,
            height: None,
            publication_state: PublicationState::Published,
            created_at: timestamp(),
        });
        manifest.narration_bindings.push(NarrationBinding {
            id: "narration-binding".into(),
            scene_id: Some("scene-narration".into()),
            render_artifact_id: "narration-audio".into(),
            history_id: "history-narration".into(),
            generation_job_id: "job-narration".into(),
            voice_id: "af-heart".into(),
            model_id: "hexgrad/Kokoro-82M".into(),
            speaker: "af_heart".into(),
            language: "en-US".into(),
            script_sha256: format!("{:x}", Sha256::digest(script.as_bytes())),
            created_at: timestamp(),
        });
        manifest.validate_strict().unwrap();

        manifest.reviewed_scenes[0].script = "The script changed.".into();
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidNarration);
        assert_eq!(
            error.field.as_deref(),
            Some("narration_bindings.script_sha256")
        );

        manifest.reviewed_scenes[0].script = script.into();
        manifest.render_artifacts[0].mime_type = "video/mp4".into();
        manifest.render_artifacts[0].width = Some(1080);
        manifest.render_artifacts[0].height = Some(1920);
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidNarration);
    }

    #[test]
    fn playback_rate_requires_exact_integer_microsecond_relation() {
        let mut clip = clip("clip-1", 0, 0, 1_000_000);
        clip.playback_rate = RationalRate {
            numerator: 2,
            denominator: 1,
        };
        let error = clip.validate().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DurationMismatch);
        clip.timeline_duration_us = Microseconds(500_000);
        clip.validate().unwrap();
    }

    #[test]
    fn playback_rate_enforces_inclusive_product_bounds() {
        for rate in [MIN_PLAYBACK_RATE, RationalRate::ONE, MAX_PLAYBACK_RATE] {
            rate.validate().unwrap();
        }

        for rate in [
            RationalRate {
                numerator: 1,
                denominator: 9,
            },
            RationalRate {
                numerator: 9,
                denominator: 1,
            },
        ] {
            let error = rate.validate().unwrap_err();
            assert_eq!(error.code, VideoErrorCode::DurationMismatch);
        }
    }

    #[test]
    fn source_and_timeline_duration_enforce_inclusive_product_bounds() {
        let mut probe = source().probe;
        probe.duration_us = Microseconds(MAX_SOURCE_DURATION_US);
        probe.validate().unwrap();
        probe.duration_us = Microseconds(MAX_SOURCE_DURATION_US + 1);
        let error = probe.validate().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidAsset);
        assert_eq!(error.field.as_deref(), Some("media.duration_us"));

        let template = manifest();
        VideoProjectManifest::new(
            "project-six-hours",
            "Six-hour project",
            RationalFrameRate::FPS_30,
            Microseconds(MAX_TIMELINE_DURATION_US),
            template.layout.clone(),
            template.audio_mix.clone(),
            timestamp(),
        )
        .unwrap();
        let error = VideoProjectManifest::new(
            "project-too-long",
            "Overlong project",
            RationalFrameRate::FPS_30,
            Microseconds(MAX_TIMELINE_DURATION_US + 1),
            template.layout,
            template.audio_mix,
            timestamp(),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidTimestamp);
        assert_eq!(error.field.as_deref(), Some("timeline_duration_us"));
    }

    #[test]
    fn rational_and_timestamp_representations_are_canonical() {
        assert_eq!(
            RationalFrameRate {
                numerator: 60,
                denominator: 2,
            }
            .validate()
            .unwrap_err()
            .code,
            VideoErrorCode::InvalidFrameRate
        );
        let mut manifest = manifest();
        manifest.updated_at = "2026-99-99T99:99:99Z".into();
        assert_eq!(
            manifest.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidTimestamp
        );
    }

    #[test]
    fn parent_traversal_is_never_a_managed_path() {
        let mut asset = source();
        asset.managed_path = "video/../../etc/passwd".into();
        assert_eq!(
            asset.validate().unwrap_err().code,
            VideoErrorCode::InvalidAsset
        );
    }

    #[test]
    fn media_sample_ranges_are_half_open_but_edit_endpoints_are_explicit() {
        let range = TimeRange::new(10, 20).unwrap();
        assert!(range.contains(Microseconds(10)));
        assert!(range.contains(Microseconds(19)));
        assert!(!range.contains(Microseconds(20)));
        assert!(range.contains_endpoint(Microseconds(20)));
    }

    #[test]
    fn artifact_backed_clips_must_fit_declared_artifact_duration() {
        let mut manifest = manifest();
        manifest.render_artifacts.push(RenderArtifact {
            id: "artifact-1".into(),
            role: RenderArtifactRole::SceneSegment,
            scene_id: None,
            managed_path: "renders/artifact-1.mp4".into(),
            sha256: hash('b'),
            cache_key: hash('c'),
            mime_type: "video/mp4".into(),
            duration_us: Some(Microseconds(500_000)),
            width: Some(1080),
            height: Some(1920),
            publication_state: PublicationState::Published,
            created_at: timestamp(),
        });
        manifest.tracks[0].clips[0].media = MediaReference {
            source_asset_id: None,
            render_artifact_id: Some("artifact-1".into()),
        };
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidTrack);
        assert_eq!(error.field.as_deref(), Some("tracks.clips.source_range"));
    }

    #[test]
    fn timeline_artifacts_require_duration_and_a_compatible_stream_kind() {
        let mut manifest = manifest();
        manifest.render_artifacts.push(RenderArtifact {
            id: "artifact-1".into(),
            role: RenderArtifactRole::SceneSegment,
            scene_id: None,
            managed_path: "renders/artifact-1.wav".into(),
            sha256: hash('b'),
            cache_key: hash('c'),
            mime_type: "audio/wav".into(),
            duration_us: None,
            width: None,
            height: None,
            publication_state: PublicationState::Published,
            created_at: timestamp(),
        });
        manifest.tracks[0].clips[0].media = MediaReference {
            source_asset_id: None,
            render_artifact_id: Some("artifact-1".into()),
        };
        assert_eq!(
            manifest.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidArtifact
        );

        manifest.render_artifacts[0].duration_us = Some(Microseconds(3_000_000));
        assert_eq!(
            manifest.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidTrack
        );

        manifest.render_artifacts[0].mime_type = "video/mp4".into();
        manifest.render_artifacts[0].width = Some(1080);
        manifest.render_artifacts[0].height = Some(1920);
        manifest.render_artifacts[0].publication_state = PublicationState::Staged;
        assert_eq!(
            manifest.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidArtifact
        );
    }

    #[test]
    fn track_stream_kinds_and_audio_mix_references_are_enforced() {
        let mut no_audio = manifest();
        no_audio.tracks[0].kind = TrackKind::Audio;
        no_audio.source_assets[0].probe.has_audio = false;
        assert_eq!(
            no_audio.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidTrack
        );

        let mut mixed_as_video = manifest();
        mixed_as_video.audio_mix.tracks.push(AudioMixTrack {
            track_id: "video-main".into(),
            gain_db_milli: 0,
            pan_milli: 0,
            ducking: None,
        });
        assert_eq!(
            mixed_as_video.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidAudioMix
        );

        let mut invalid_sidechain = manifest();
        invalid_sidechain.tracks.push(TimelineTrack {
            id: "audio-main".into(),
            kind: TrackKind::Audio,
            clips: vec![],
            preserve_gaps: false,
        });
        invalid_sidechain.audio_mix.tracks.push(AudioMixTrack {
            track_id: "audio-main".into(),
            gain_db_milli: 0,
            pan_milli: 0,
            ducking: Some(DuckingSpec {
                sidechain_track_id: "video-main".into(),
                reduction_db_milli: -12_000,
                attack_us: Microseconds(10_000),
                release_us: Microseconds(100_000),
            }),
        });
        assert_eq!(
            invalid_sidechain.validate_strict().unwrap_err().code,
            VideoErrorCode::InvalidAudioMix
        );
    }
}
