//! Deterministic transcript normalization, clip analysis, and reviewed timeline planning.
//!
//! Codex may research, write, and choose among candidates, but source-clock timing and
//! timeline mutations remain local, versioned, and reproducible. This module deliberately
//! accepts the same JSON evidence emitted by soundAr's existing Whisper runtime and by the
//! supported local faster-whisper adapters.

use super::contracts::{
    AudioMix, AudioMixTrack, CandidateStatus, CaptionCue, ClipCandidate, GapReason, MediaReference,
    Microseconds, RationalRate, ReviewState, ReviewedScene, TimeRange, TimelineClip, TimelineGap,
    TimelineTrack, TrackKind, TranscriptSegment, TranscriptTimingSource, TranscriptVersion,
    TranscriptWord, Validate, VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
};
use super::timeline::quantize_range_outward;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_TRANSCRIPT_ITEMS: usize = 500_000;
const CLOCK_TOLERANCE_US: i64 = 20_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptImportRequest {
    pub source_asset_id: String,
    pub source_clock_duration_us: Microseconds,
    pub timing_source: TranscriptTimingSource,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePolicy {
    pub minimum_duration_us: Microseconds,
    pub target_duration_us: Microseconds,
    pub maximum_duration_us: Microseconds,
    pub maximum_candidates: usize,
}

impl Default for CandidatePolicy {
    fn default() -> Self {
        Self {
            minimum_duration_us: Microseconds(4_000_000),
            target_duration_us: Microseconds(30_000_000),
            maximum_duration_us: Microseconds(60_000_000),
            maximum_candidates: 12,
        }
    }
}

impl Validate for CandidatePolicy {
    fn validate(&self) -> VideoResult<()> {
        if self.minimum_duration_us.0 <= 0
            || self.target_duration_us < self.minimum_duration_us
            || self.maximum_duration_us < self.target_duration_us
            || self.maximum_duration_us.0 > 600_000_000
            || self.maximum_candidates == 0
            || self.maximum_candidates > 100
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCandidate,
                "candidate durations must satisfy 0 < minimum <= target <= maximum <= 10 minutes, with 1..=100 results",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAnalysis {
    pub transcript_id: String,
    pub candidates: Vec<ClipCandidate>,
    /// Stable source-range fingerprints make rejected suggestions stay rejected across
    /// analysis reruns without coupling that protection to transient candidate IDs.
    pub source_range_fingerprints: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenePlanRequest {
    pub selected_candidate_ids: Vec<String>,
    pub caption_style_id: String,
    #[serde(default = "zero_microseconds")]
    pub inter_scene_gap_us: Microseconds,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenePlan {
    pub timeline_duration_us: Microseconds,
    pub candidates: Vec<ClipCandidate>,
    pub reviewed_scenes: Vec<ReviewedScene>,
    pub tracks: Vec<TimelineTrack>,
    pub gaps: Vec<TimelineGap>,
    pub captions: Vec<CaptionCue>,
    pub audio_mix: AudioMix,
}

/// Normalize timestamped local transcription evidence onto the exact probed source clock.
/// Missing or invalid timestamps are rejected rather than guessed; callers can then run the
/// supported local transcription fallback.
pub fn transcript_from_runtime_json(
    evidence: &Value,
    request: &TranscriptImportRequest,
) -> VideoResult<TranscriptVersion> {
    if request.source_clock_duration_us.0 <= 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "the probed source clock must be positive",
        )
        .at("source_clock_duration_us"));
    }

    let raw_segments = evidence
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "timestamped transcript segments are required",
            )
            .at("segments")
        })?;
    if raw_segments.is_empty() || raw_segments.len() > MAX_TRANSCRIPT_ITEMS {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "transcript segments must contain 1..=500000 timestamped items",
        )
        .at("segments"));
    }

    let raw_words = evidence
        .get("words")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if raw_words.len() > MAX_TRANSCRIPT_ITEMS {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "transcript words exceed the supported item limit",
        )
        .at("words"));
    }

    let mut words = Vec::with_capacity(raw_words.len());
    for (index, raw) in raw_words.iter().enumerate() {
        let text = normalized_text(raw.get("text").and_then(Value::as_str).unwrap_or_default());
        if text.is_empty() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "timestamped words may not be empty",
            )
            .at(format!("words.{index}.text")));
        }
        let range = parse_seconds_range(raw, request.source_clock_duration_us, "words", index)?;
        words.push(TranscriptWord {
            id: format!("word-{index:06}"),
            range,
            text,
            speaker_id: optional_text(raw, &["speaker_id", "speaker"]),
            confidence_milli: confidence_milli(raw),
        });
    }

    let mut segments = Vec::with_capacity(raw_segments.len());
    for (index, raw) in raw_segments.iter().enumerate() {
        let text = normalized_text(raw.get("text").and_then(Value::as_str).unwrap_or_default());
        if text.is_empty() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "timestamped segments may not be empty",
            )
            .at(format!("segments.{index}.text")));
        }
        let range = parse_seconds_range(raw, request.source_clock_duration_us, "segments", index)?;
        let word_ids = words
            .iter()
            .filter(|word| {
                word.range.start_us >= range.start_us && word.range.end_us <= range.end_us
            })
            .map(|word| word.id.clone())
            .collect();
        segments.push(TranscriptSegment {
            id: format!("segment-{index:06}"),
            range,
            text,
            speaker_id: optional_text(raw, &["speaker_id", "speaker"]),
            word_ids,
        });
    }

    let language = optional_text(evidence, &["detected_language", "language"]);
    let content_sha256 = transcript_content_hash(
        &request.source_asset_id,
        request.source_clock_duration_us,
        language.as_deref(),
        &segments,
        &words,
    )?;
    let transcript = TranscriptVersion {
        id: format!("transcript-{}", &content_sha256[..24]),
        source_asset_id: request.source_asset_id.clone(),
        source_clock_duration_us: request.source_clock_duration_us,
        language,
        timing_source: request.timing_source.clone(),
        preserved_source_gaps: true,
        segments,
        words,
        content_sha256,
        created_at: request.created_at.clone(),
    };
    transcript.validate()?;
    Ok(transcript)
}

/// Produce deterministic, non-overlapping suggestions. A rejected source fingerprint can be
/// supplied on later runs so the same content is not quietly proposed again.
pub fn identify_clip_candidates(
    transcript: &TranscriptVersion,
    policy: &CandidatePolicy,
    excluded_source_fingerprints: &BTreeSet<String>,
) -> VideoResult<CandidateAnalysis> {
    transcript.validate()?;
    policy.validate()?;

    let mut candidates = Vec::new();
    let mut fingerprints = BTreeMap::new();
    let mut start_index = 0usize;
    while start_index < transcript.segments.len() && candidates.len() < policy.maximum_candidates {
        let start = transcript.segments[start_index].range.start_us;
        let mut end_index = start_index;
        while end_index + 1 < transcript.segments.len() {
            let next_end = transcript.segments[end_index + 1].range.end_us;
            if next_end.0 - start.0 > policy.maximum_duration_us.0 {
                break;
            }
            end_index += 1;
            if next_end.0 - start.0 >= policy.target_duration_us.0 {
                break;
            }
        }

        let end = transcript.segments[end_index].range.end_us;
        let duration = end.0 - start.0;
        if duration < policy.minimum_duration_us.0 {
            break;
        }
        let selected = &transcript.segments[start_index..=end_index];
        let range = TimeRange::new(start.0, end.0)?;
        let fingerprint = source_range_fingerprint(
            &transcript.source_asset_id,
            range,
            selected.iter().map(|segment| segment.id.as_str()),
        );
        if !excluded_source_fingerprints.contains(&fingerprint) {
            let text = selected
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let id = format!("candidate-{}", &fingerprint[..24]);
            let title = candidate_title(&text);
            let score_milli = candidate_score(&text, duration, policy.target_duration_us.0);
            candidates.push(ClipCandidate {
                id: id.clone(),
                source_asset_id: transcript.source_asset_id.clone(),
                source_range: range,
                title,
                rationale: candidate_rationale(&text, duration),
                transcript_segment_ids: selected.iter().map(|segment| segment.id.clone()).collect(),
                score_milli,
                status: CandidateStatus::Proposed,
            });
            fingerprints.insert(id, fingerprint);
        }
        start_index = end_index + 1;
    }

    for candidate in &candidates {
        candidate.validate()?;
    }
    Ok(CandidateAnalysis {
        transcript_id: transcript.id.clone(),
        candidates,
        source_range_fingerprints: fingerprints,
    })
}

/// Build the exact reviewed timeline used by preview and final export. Selected source ranges
/// are expanded to frame boundaries where possible, while every transcript/caption timestamp is
/// mapped from the original source clock into its scene's timeline position.
pub fn plan_reviewed_timeline(
    manifest: &VideoProjectManifest,
    request: &ScenePlanRequest,
) -> VideoResult<ScenePlan> {
    manifest.validate_strict()?;
    if request.selected_candidate_ids.is_empty() || request.selected_candidate_ids.len() > 100 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidScene,
            "a plan requires 1..=100 selected candidates",
        )
        .at("selected_candidate_ids"));
    }
    if request.caption_style_id.is_empty() || request.caption_style_id.len() > 128 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCaption,
            "caption_style_id must be a bounded identifier",
        )
        .at("caption_style_id"));
    }
    if request.inter_scene_gap_us.0 < 0 || request.inter_scene_gap_us.0 > 10_000_000 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidGap,
            "inter-scene gaps must be between zero and ten seconds",
        ));
    }

    let transcript = manifest.transcript.as_ref().ok_or_else(|| {
        VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "analysis must produce a transcript before scene planning",
        )
    })?;
    let candidate_by_id = manifest
        .candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let source_by_id = manifest
        .source_assets
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let segment_by_id = transcript
        .segments
        .iter()
        .map(|segment| (segment.id.as_str(), segment))
        .collect::<BTreeMap<_, _>>();
    let mut selected_ids = BTreeSet::new();
    let mut selected = Vec::with_capacity(request.selected_candidate_ids.len());
    for id in &request.selected_candidate_ids {
        if !selected_ids.insert(id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::DuplicateId,
                format!("candidate {id} was selected more than once"),
            ));
        }
        let candidate = candidate_by_id.get(id.as_str()).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::MissingReference,
                format!("selected candidate {id} does not exist"),
            )
        })?;
        if matches!(candidate.status, CandidateStatus::Rejected) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCandidate,
                format!("rejected candidate {id} cannot be placed on the timeline"),
            ));
        }
        selected.push(*candidate);
    }

    let mut planned_candidates = manifest.candidates.clone();
    for candidate in &mut planned_candidates {
        candidate.status = if selected_ids.contains(candidate.id.as_str()) {
            CandidateStatus::Accepted
        } else if matches!(candidate.status, CandidateStatus::Accepted) {
            CandidateStatus::Proposed
        } else {
            candidate.status.clone()
        };
    }

    // Track existence is a property of the complete plan, not whichever source
    // happens to precede an inter-scene boundary. Once a track exists, every
    // scene and transition must contribute either a clip or an explicit gap so
    // preserve_gaps remains a truthful full-timeline contract for mixed media.
    let includes_video_track = selected.iter().any(|candidate| {
        source_by_id
            .get(candidate.source_asset_id.as_str())
            .is_some_and(|source| source.probe.has_video)
    });
    let includes_audio_track = selected.iter().any(|candidate| {
        source_by_id
            .get(candidate.source_asset_id.as_str())
            .is_some_and(|source| source.probe.has_audio)
    });

    let mut scenes = Vec::with_capacity(selected.len());
    let mut video_clips = Vec::new();
    let mut audio_clips = Vec::new();
    let mut captions = Vec::new();
    let mut gaps = Vec::new();
    let mut cursor = Microseconds::ZERO;

    for (index, candidate) in selected.iter().enumerate() {
        let source = source_by_id
            .get(candidate.source_asset_id.as_str())
            .ok_or_else(|| {
                VideoError::new(
                    VideoErrorCode::MissingReference,
                    "selected candidate references a missing source asset",
                )
            })?;
        let quantized = quantize_range_outward(candidate.source_range, manifest.frame_rate)?;
        let source_start = quantized.start_us.max(Microseconds::ZERO);
        let source_end = quantized.end_us.min(source.probe.duration_us);
        let source_range = TimeRange::new(source_start.0, source_end.0)?;
        let duration = source_range.duration()?;
        let candidate_suffix = format!("{:x}", Sha256::digest(candidate.id.as_bytes()));
        let scene_id = format!("scene-{:03}-{}", index + 1, &candidate_suffix[..8]);
        scenes.push(ReviewedScene {
            id: scene_id.clone(),
            candidate_id: Some(candidate.id.clone()),
            source_asset_id: Some(candidate.source_asset_id.clone()),
            source_range: Some(source_range),
            timeline_start_us: cursor,
            timeline_duration_us: duration,
            title: candidate.title.clone(),
            script: candidate
                .transcript_segment_ids
                .iter()
                .filter_map(|id| segment_by_id.get(id.as_str()))
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            review_state: ReviewState::NeedsReview,
            revision: 1,
        });

        let make_clip = |suffix: &str| TimelineClip {
            id: format!("clip-{suffix}-{:03}", index + 1),
            scene_id: Some(scene_id.clone()),
            media: MediaReference {
                source_asset_id: Some(candidate.source_asset_id.clone()),
                render_artifact_id: None,
            },
            source_range,
            timeline_start_us: cursor,
            timeline_duration_us: duration,
            playback_rate: RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        };
        let scene_end = cursor.checked_add(duration)?;
        if source.probe.has_video {
            video_clips.push(make_clip("video"));
        } else if includes_video_track {
            gaps.push(TimelineGap {
                id: format!("gap-video-scene-{:03}", index + 1),
                track_id: "video-main".into(),
                range: TimeRange::new(cursor.0, scene_end.0)?,
                reason: GapReason::Padding,
                source_asset_id: None,
                source_range: None,
            });
        }
        if source.probe.has_audio {
            audio_clips.push(make_clip("audio"));
        } else if includes_audio_track {
            gaps.push(TimelineGap {
                id: format!("gap-audio-scene-{:03}", index + 1),
                track_id: "audio-main".into(),
                range: TimeRange::new(cursor.0, scene_end.0)?,
                reason: GapReason::Padding,
                source_asset_id: None,
                source_range: None,
            });
        }

        for segment_id in &candidate.transcript_segment_ids {
            let Some(segment) = segment_by_id.get(segment_id.as_str()) else {
                continue;
            };
            let clipped_start = segment.range.start_us.max(source_range.start_us);
            let clipped_end = segment.range.end_us.min(source_range.end_us);
            if clipped_end <= clipped_start {
                continue;
            }
            let timeline_start =
                cursor.checked_add(Microseconds(clipped_start.0 - source_range.start_us.0))?;
            let timeline_end =
                cursor.checked_add(Microseconds(clipped_end.0 - source_range.start_us.0))?;
            captions.push(CaptionCue {
                id: format!("caption-{:03}-{:06}", index + 1, captions.len() + 1),
                range: TimeRange::new(timeline_start.0, timeline_end.0)?,
                text: segment.text.clone(),
                style_id: request.caption_style_id.clone(),
                speaker_id: segment.speaker_id.clone(),
                transcript_segment_id: Some(segment.id.clone()),
                scene_id: Some(scene_id.clone()),
            });
        }

        cursor = scene_end;
        if index + 1 < selected.len() && request.inter_scene_gap_us.0 > 0 {
            let gap_end = cursor.checked_add(request.inter_scene_gap_us)?;
            if includes_video_track {
                gaps.push(TimelineGap {
                    id: format!("gap-video-transition-{:03}", index + 1),
                    track_id: "video-main".into(),
                    range: TimeRange::new(cursor.0, gap_end.0)?,
                    reason: GapReason::Transition,
                    source_asset_id: None,
                    source_range: None,
                });
            }
            if includes_audio_track {
                gaps.push(TimelineGap {
                    id: format!("gap-audio-transition-{:03}", index + 1),
                    track_id: "audio-main".into(),
                    range: TimeRange::new(cursor.0, gap_end.0)?,
                    reason: GapReason::Editorial,
                    source_asset_id: None,
                    source_range: None,
                });
            }
            cursor = gap_end;
        }
    }

    let mut tracks = Vec::new();
    if !video_clips.is_empty() {
        tracks.push(TimelineTrack {
            id: "video-main".into(),
            kind: TrackKind::Video,
            clips: video_clips,
            preserve_gaps: true,
        });
    }
    if !audio_clips.is_empty() {
        tracks.push(TimelineTrack {
            id: "audio-main".into(),
            kind: TrackKind::Audio,
            clips: audio_clips,
            preserve_gaps: true,
        });
    }
    let audio_mix = AudioMix {
        target_lufs_milli: manifest.audio_mix.target_lufs_milli,
        true_peak_db_milli: manifest.audio_mix.true_peak_db_milli,
        tracks: tracks
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::Audio))
            .map(|track| AudioMixTrack {
                track_id: track.id.clone(),
                gain_db_milli: 0,
                pan_milli: 0,
                ducking: None,
            })
            .collect(),
    };
    let plan = ScenePlan {
        timeline_duration_us: cursor,
        candidates: planned_candidates,
        reviewed_scenes: scenes,
        tracks,
        gaps,
        captions,
        audio_mix,
    };
    validate_scene_plan(manifest, &plan)?;
    Ok(plan)
}

/// Apply a locally validated plan without touching unrelated source, rights, provenance, or
/// render history fields. The caller remains responsible for recording the durable revision.
pub fn apply_scene_plan(manifest: &mut VideoProjectManifest, plan: ScenePlan) -> VideoResult<()> {
    manifest.timeline_duration_us = plan.timeline_duration_us;
    manifest.candidates = plan.candidates;
    manifest.reviewed_scenes = plan.reviewed_scenes;
    manifest.tracks = plan.tracks;
    manifest.gaps = plan.gaps;
    manifest.captions = plan.captions;
    manifest.audio_mix = plan.audio_mix;
    manifest.validate_strict()
}

pub fn source_range_fingerprint<'a>(
    source_asset_id: &str,
    range: TimeRange,
    segment_ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"soundar-video-source-range-v1\0");
    digest.update(source_asset_id.as_bytes());
    digest.update(b"\0");
    digest.update(range.start_us.0.to_be_bytes());
    digest.update(range.end_us.0.to_be_bytes());
    for segment_id in segment_ids {
        digest.update(b"\0");
        digest.update(segment_id.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn validate_scene_plan(manifest: &VideoProjectManifest, plan: &ScenePlan) -> VideoResult<()> {
    let mut candidate = manifest.clone();
    candidate.timeline_duration_us = plan.timeline_duration_us;
    candidate.candidates = plan.candidates.clone();
    candidate.reviewed_scenes = plan.reviewed_scenes.clone();
    candidate.tracks = plan.tracks.clone();
    candidate.gaps = plan.gaps.clone();
    candidate.captions = plan.captions.clone();
    candidate.audio_mix = plan.audio_mix.clone();
    candidate.validate_strict()
}

fn parse_seconds_range(
    value: &Value,
    source_duration: Microseconds,
    collection: &str,
    index: usize,
) -> VideoResult<TimeRange> {
    let start = numeric_field(value, &["start_seconds", "start"]);
    let end = numeric_field(value, &["end_seconds", "end"]);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "timestamped text requires numeric start and end seconds",
        )
        .at(format!("{collection}.{index}")));
    };
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err(VideoError::new(
            VideoErrorCode::InvalidTranscript,
            "timestamp bounds must be finite and satisfy 0 <= start < end",
        )
        .at(format!("{collection}.{index}")));
    }
    let start_us = seconds_to_microseconds(start)?;
    let mut end_us = seconds_to_microseconds(end)?;
    if end_us > source_duration.0 {
        if end_us - source_duration.0 <= CLOCK_TOLERANCE_US {
            end_us = source_duration.0;
        } else {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTranscript,
                "timestamp extends beyond the probed source clock",
            )
            .at(format!("{collection}.{index}")));
        }
    }
    TimeRange::new(start_us, end_us)
}

fn seconds_to_microseconds(seconds: f64) -> VideoResult<i64> {
    let micros = seconds * 1_000_000.0;
    if !micros.is_finite() || micros > i64::MAX as f64 {
        return Err(VideoError::new(
            VideoErrorCode::ArithmeticOverflow,
            "seconds could not be represented on the microsecond clock",
        ));
    }
    Ok(micros.round() as i64)
}

fn numeric_field(value: &Value, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_f64))
}

fn optional_text(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(Value::as_str)
            .map(normalized_text)
            .filter(|text| !text.is_empty())
    })
}

fn confidence_milli(value: &Value) -> Option<u16> {
    numeric_field(value, &["confidence", "probability", "alignment_score"]).and_then(|number| {
        (number.is_finite() && (0.0..=1.0).contains(&number))
            .then_some((number * 1_000.0).round() as u16)
    })
}

const fn zero_microseconds() -> Microseconds {
    Microseconds::ZERO
}

fn normalized_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn transcript_content_hash(
    source_asset_id: &str,
    duration: Microseconds,
    language: Option<&str>,
    segments: &[TranscriptSegment],
    words: &[TranscriptWord],
) -> VideoResult<String> {
    let canonical = serde_json::to_vec(&json!({
        "schema": 1,
        "source_asset_id": source_asset_id,
        "source_clock_duration_us": duration,
        "language": language,
        "segments": segments,
        "words": words,
    }))
    .map_err(|error| {
        VideoError::new(
            VideoErrorCode::InvalidTranscript,
            format!("could not canonicalize transcript evidence: {error}"),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn candidate_title(text: &str) -> String {
    let words = text.split_whitespace().take(9).collect::<Vec<_>>();
    let mut title = words.join(" ");
    if text.split_whitespace().count() > words.len() {
        title.push('…');
    }
    if title.len() > 500 {
        title.truncate(500);
    }
    title
}

fn candidate_score(text: &str, duration_us: i64, target_us: i64) -> u16 {
    let word_count = text.split_whitespace().count() as i64;
    let duration_distance = (duration_us - target_us).unsigned_abs() as i64;
    let duration_score = 380i64.saturating_sub(duration_distance.saturating_mul(300) / target_us);
    let density_score = (word_count.saturating_mul(4)).min(260);
    let hook_score = i64::from(text.contains('?')) * 90
        + i64::from(text.chars().any(|character| character.is_ascii_digit())) * 70
        + i64::from(text.contains('!')) * 40;
    (200 + duration_score + density_score + hook_score).clamp(0, 1_000) as u16
}

fn candidate_rationale(text: &str, duration_us: i64) -> String {
    let duration_seconds = duration_us as f64 / 1_000_000.0;
    let hook = if text.contains('?') {
        "It contains a clear question-led hook"
    } else if text.chars().any(|character| character.is_ascii_digit()) {
        "It contains a concrete, specific hook"
    } else {
        "It forms a self-contained spoken idea"
    };
    format!(
        "{hook} and fits a {:.1}-second edit while retaining the original source-clock pauses.",
        duration_seconds
    )
}

#[cfg(test)]
mod tests {
    use super::super::contracts::{
        AudioMix, CanvasMode, CanvasSpec, LayoutPlan, MediaProbe, NormalizedRect, Provenance,
        ProvenanceKind, RationalFrameRate, SourceAsset, SourceAssetKind,
    };
    use super::*;
    use std::collections::BTreeMap;

    fn timestamp() -> String {
        "2026-08-27T18:30:00.000Z".into()
    }

    fn transcript_request() -> TranscriptImportRequest {
        TranscriptImportRequest {
            source_asset_id: "source-1".into(),
            source_clock_duration_us: Microseconds(12_000_000),
            timing_source: TranscriptTimingSource::FasterWhisper,
            created_at: timestamp(),
        }
    }

    fn evidence() -> Value {
        json!({
            "detected_language": "en",
            "segments": [
                {"start_seconds": 1.0, "end_seconds": 4.0, "text": "Why does a strong opening matter?"},
                {"start_seconds": 5.0, "end_seconds": 8.0, "text": "It earns the next few seconds."},
                {"start_seconds": 9.0, "end_seconds": 11.5, "text": "Then one concrete example makes it memorable."}
            ],
            "words": [
                {"start_seconds": 1.0, "end_seconds": 1.4, "text": "Why", "confidence": 0.98},
                {"start_seconds": 1.5, "end_seconds": 2.0, "text": "does", "confidence": 0.96},
                {"start_seconds": 5.0, "end_seconds": 5.4, "text": "It", "confidence": 0.95}
            ]
        })
    }

    fn manifest_with_transcript(transcript: TranscriptVersion) -> VideoProjectManifest {
        let layout = LayoutPlan {
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
            background_rgba: [16, 16, 16, 255],
            elements: vec![],
        };
        let mut manifest = VideoProjectManifest::new(
            "project-1",
            "Source reel",
            RationalFrameRate::FPS_30,
            Microseconds(12_000_000),
            layout,
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
            managed_path: "projects/project-1/source.mp4".into(),
            sha256: "a".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(12_000_000),
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(RationalFrameRate::FPS_30),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: timestamp(),
                producer: "soundAr Video Studio".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        manifest.transcript = Some(transcript);
        manifest.validate_strict().unwrap();
        manifest
    }

    #[test]
    fn runtime_transcript_preserves_measured_source_gaps_and_is_stable() {
        let first = transcript_from_runtime_json(&evidence(), &transcript_request()).unwrap();
        let second = transcript_from_runtime_json(&evidence(), &transcript_request()).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.content_sha256, second.content_sha256);
        assert_eq!(first.segments[0].range.start_us, Microseconds(1_000_000));
        assert_eq!(first.segments[1].range.start_us, Microseconds(5_000_000));
        assert!(first.preserved_source_gaps);
        assert_eq!(first.segments[0].word_ids.len(), 2);
    }

    #[test]
    fn invalid_external_timing_is_rejected_for_local_fallback() {
        let invalid = json!({
            "segments": [{"start": 11.0, "end": 13.0, "text": "outside"}]
        });
        let error = transcript_from_runtime_json(&invalid, &transcript_request()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidTranscript);

        let missing = json!({"text": "untimed transcript"});
        let error = transcript_from_runtime_json(&missing, &transcript_request()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidTranscript);
    }

    #[test]
    fn rejected_source_ranges_do_not_reappear_after_analysis() {
        let transcript = transcript_from_runtime_json(&evidence(), &transcript_request()).unwrap();
        let policy = CandidatePolicy {
            minimum_duration_us: Microseconds(2_000_000),
            target_duration_us: Microseconds(5_000_000),
            maximum_duration_us: Microseconds(8_000_000),
            maximum_candidates: 10,
        };
        let initial = identify_clip_candidates(&transcript, &policy, &BTreeSet::new()).unwrap();
        assert_eq!(initial.candidates.len(), 2);
        let excluded =
            BTreeSet::from([initial.source_range_fingerprints[&initial.candidates[0].id].clone()]);
        let rerun = identify_clip_candidates(&transcript, &policy, &excluded).unwrap();
        assert_eq!(rerun.candidates.len(), 1);
        assert_eq!(rerun.candidates[0].id, initial.candidates[1].id);
    }

    #[test]
    fn reviewed_plan_maps_captions_and_keeps_internal_source_silence() {
        let transcript = transcript_from_runtime_json(&evidence(), &transcript_request()).unwrap();
        let analysis = identify_clip_candidates(
            &transcript,
            &CandidatePolicy {
                minimum_duration_us: Microseconds(2_000_000),
                target_duration_us: Microseconds(5_000_000),
                maximum_duration_us: Microseconds(8_000_000),
                maximum_candidates: 10,
            },
            &BTreeSet::new(),
        )
        .unwrap();
        let mut manifest = manifest_with_transcript(transcript);
        manifest.candidates = analysis.candidates;
        manifest.validate_strict().unwrap();
        let plan = plan_reviewed_timeline(
            &manifest,
            &ScenePlanRequest {
                selected_candidate_ids: manifest
                    .candidates
                    .iter()
                    .map(|candidate| candidate.id.clone())
                    .collect(),
                caption_style_id: "caption-calm".into(),
                inter_scene_gap_us: Microseconds(250_000),
            },
        )
        .unwrap();
        assert_eq!(plan.reviewed_scenes.len(), 2);
        assert_eq!(plan.tracks.len(), 2);
        assert_eq!(plan.gaps.len(), 2);
        assert_eq!(plan.captions.len(), 3);
        // The first source clip spans 1s..8s, including the measured 4s..5s pause.
        assert_eq!(
            plan.tracks[0].clips[0].source_range.duration().unwrap().0,
            7_000_000
        );
        let mut applied = manifest;
        apply_scene_plan(&mut applied, plan).unwrap();
        applied.validate_strict().unwrap();
    }

    #[test]
    fn mixed_stream_sources_form_complete_partitions_on_every_track() {
        let transcript = transcript_from_runtime_json(&evidence(), &transcript_request()).unwrap();
        let mut manifest = manifest_with_transcript(transcript);
        manifest.source_assets[0].probe.has_audio = false;
        manifest.source_assets.push(SourceAsset {
            id: "source-audio".into(),
            kind: SourceAssetKind::LocalVideo,
            managed_path: "projects/project-1/audio.wav".into(),
            sha256: "b".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(12_000_000),
                width: None,
                height: None,
                frame_rate: None,
                has_video: false,
                has_audio: true,
                format_name: "wav".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: timestamp(),
                producer: "soundAr Video Studio".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        manifest.candidates = vec![
            ClipCandidate {
                id: "video".into(),
                source_asset_id: "source-1".into(),
                source_range: TimeRange::new(1_000_000, 4_000_000).unwrap(),
                title: "Video scene".into(),
                rationale: "Exercises a video-only timeline scene.".into(),
                transcript_segment_ids: vec!["segment-000000".into()],
                score_milli: 900,
                status: CandidateStatus::Proposed,
            },
            ClipCandidate {
                id: "audio".into(),
                source_asset_id: "source-audio".into(),
                source_range: TimeRange::new(5_000_000, 8_000_000).unwrap(),
                title: "Audio scene".into(),
                rationale: "Exercises an audio-only timeline scene.".into(),
                transcript_segment_ids: vec![],
                score_milli: 850,
                status: CandidateStatus::Proposed,
            },
        ];
        manifest.validate_strict().unwrap();

        let plan = plan_reviewed_timeline(
            &manifest,
            &ScenePlanRequest {
                selected_candidate_ids: vec!["video".into(), "audio".into()],
                caption_style_id: "caption-calm".into(),
                inter_scene_gap_us: Microseconds(250_000),
            },
        )
        .unwrap();

        assert_eq!(plan.tracks.len(), 2);
        let video = plan
            .tracks
            .iter()
            .find(|track| matches!(track.kind, TrackKind::Video))
            .unwrap();
        let audio = plan
            .tracks
            .iter()
            .find(|track| matches!(track.kind, TrackKind::Audio))
            .unwrap();
        assert_eq!(video.clips.len(), 1);
        assert_eq!(audio.clips.len(), 1);
        assert_eq!(plan.gaps.len(), 4);
        assert_eq!(
            plan.gaps
                .iter()
                .filter(|gap| gap.track_id == video.id)
                .count(),
            2
        );
        assert_eq!(
            plan.gaps
                .iter()
                .filter(|gap| gap.track_id == audio.id)
                .count(),
            2
        );

        let mut applied = manifest;
        apply_scene_plan(&mut applied, plan).unwrap();
        applied.validate_strict().unwrap();
    }
}
