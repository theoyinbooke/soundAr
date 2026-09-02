//! Checking a finished episode against what it was asked to be.
//!
//! Local speech models skip words, run words together, and mispronounce invented names, and they do
//! it silently: the audio sounds confident either way. The only reliable way to know is to listen
//! back and compare, which is exactly what soundAr already has the parts for - a local recognizer,
//! word timings, and the exact script every take was asked to speak.
//!
//! Everything here reports. Nothing here rewrites a script, re-renders a take, or adjusts a mix.
//! A check that silently repaired what it found would destroy the one thing it exists to provide,
//! which is an honest account of what the rendered episode actually contains.
//!
//! Measurements arrive from the runtime that made them. This module never estimates a loudness it
//! did not receive, and a check with no measurement is reported as unchecked rather than as passed.

use super::contracts::{
    validate_identifier, Microseconds, Validate, VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};

/// How far a rendered master may sit from its loudness target before it is worth reporting.
/// Broadcast practice treats a decibel either side as inaudible; beyond that a platform will
/// normalize the episode and change how it sounds.
pub const LOUDNESS_TOLERANCE_MILLI: i32 = 1_000;

/// How far a measured true peak may sit above the ceiling before it is worth reporting. A
/// lossy encoder and a meter disagree by a tenth of a decibel or two on the same master; that
/// is the measurement, not the mix.
pub const TRUE_PEAK_TOLERANCE_MILLI: i32 = 300;

/// How far a caption may sit from the word it belongs to before a viewer notices.
pub const CAPTION_DRIFT_TOLERANCE_US: i64 = 250_000;

/// Silence longer than this inside an episode reads as a fault rather than a beat.
pub const DEAD_AIR_THRESHOLD_US: i64 = 3_000_000;

/// What a check found.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QcFindingKind {
    /// The take does not say a word the script asked for.
    SkippedWord,
    /// The take says a word the script did not ask for.
    InsertedWord,
    /// The take says a different word in place of the scripted one. The usual shape of a
    /// mispronounced name.
    ReplacedWord,
    /// The rendered master sits outside its loudness target.
    LoudnessOffTarget,
    /// The rendered master exceeds its true-peak ceiling.
    TruePeakExceeded,
    /// A caption sits too far from the word it belongs to.
    CaptionDrift,
    /// Silence long enough to read as a fault.
    DeadAir,
    /// The performed episode sits outside the length its show asked for.
    DurationOffTarget,
    /// A vocal cue was read as its word - the voice said "laughter" instead of laughing.
    SpokenCue,
    /// A vocal cue the cast voice cannot perform was removed rather than spoken.
    DroppedCue,
}

/// How much a finding matters.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QcSeverity {
    /// Worth a look; the episode is publishable as it stands.
    Notice,
    /// A listener will notice this.
    Warning,
    /// The episode does not say what it was asked to say, or will be altered on publication.
    Blocking,
}

/// One reviewable finding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QcFinding {
    pub id: String,
    pub kind: QcFindingKind,
    pub severity: QcSeverity,
    /// The line this finding belongs to, when it belongs to one. Loudness belongs to the master.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// What was found, in the terms the user wrote it in.
    pub detail: String,
    /// Where in the episode, when the finding has a position.
    #[serde(default)]
    pub at_us: Option<Microseconds>,
}

impl Validate for QcFinding {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "quality.findings.id")?;
        if let Some(turn_id) = &self.turn_id {
            validate_identifier(turn_id, "quality.findings.turn_id")?;
        }
        if self.detail.trim().is_empty() || self.detail.len() > 2_000 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidQualityReport,
                "a finding must say what it found, in at most 2000 bytes",
            )
            .at("quality.findings.detail"));
        }
        Ok(())
    }
}

/// Loudness as the runtime measured it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoudnessMeasurement {
    pub integrated_lufs_milli: i32,
    pub true_peak_db_milli: i32,
}

/// The result of checking one episode.
///
/// `checked_turns` and `unchecked_turns` are reported separately because a turn nobody listened
/// back to is not a turn that passed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QcReport {
    pub findings: Vec<QcFinding>,
    pub checked_turns: Vec<String>,
    pub unchecked_turns: Vec<String>,
    /// False when no loudness measurement was supplied, so the report cannot claim the master is
    /// within its target.
    pub loudness_checked: bool,
}

impl QcReport {
    pub fn blocking(&self) -> Vec<&QcFinding> {
        self.findings
            .iter()
            .filter(|finding| matches!(finding.severity, QcSeverity::Blocking))
            .collect()
    }

    /// Whether the episode is clear to publish. A report with unchecked turns is never clear,
    /// because the check simply did not happen for them.
    pub fn is_clear(&self) -> bool {
        self.blocking().is_empty() && self.unchecked_turns.is_empty() && self.loudness_checked
    }
}

/// How one word in the script relates to what the take actually said.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WordDifference {
    Skipped {
        expected: String,
        at_index: usize,
    },
    Inserted {
        heard: String,
        at_index: usize,
    },
    Replaced {
        expected: String,
        heard: String,
        at_index: usize,
    },
}

/// Compare what a take was asked to say with what it actually said.
///
/// Comparison is on normalized words: a recognizer does not reproduce punctuation or capitalization,
/// and reporting those as errors would bury the real ones. Everything else is reported, because a
/// skipped word is a skipped word whether or not it seemed important.
pub fn diff_spoken_words(expected: &str, heard: &str) -> Vec<WordDifference> {
    let expected_words = normalize_words(expected);
    let heard_words = normalize_words(heard);
    if expected_words.is_empty() && heard_words.is_empty() {
        return Vec::new();
    }
    // A recogniser writes "good night" as "goodnight" and "smartphone" as "smart phone"; either
    // way the same words were said. Two neighbours on one side that spell one word on the other
    // are compared as that word.
    let heard_words = join_compounds(heard_words, &expected_words);
    let expected_words = join_compounds(expected_words, &heard_words);

    // Longest common subsequence over the normalized words. The alignment is what turns a raw
    // mismatch into "this word was replaced by that one" rather than a wall of insertions and
    // deletions that nobody can act on.
    let rows = expected_words.len() + 1;
    let columns = heard_words.len() + 1;
    let mut lengths = vec![0usize; rows * columns];
    for row in (0..expected_words.len()).rev() {
        for column in (0..heard_words.len()).rev() {
            lengths[row * columns + column] =
                if expected_words[row].normalized == heard_words[column].normalized {
                    lengths[(row + 1) * columns + column + 1] + 1
                } else {
                    lengths[(row + 1) * columns + column].max(lengths[row * columns + column + 1])
                };
        }
    }

    let mut differences = Vec::new();
    let (mut row, mut column) = (0usize, 0usize);
    while row < expected_words.len() && column < heard_words.len() {
        if expected_words[row].normalized == heard_words[column].normalized {
            row += 1;
            column += 1;
        } else if lengths[(row + 1) * columns + column] >= lengths[row * columns + column + 1] {
            // A deletion immediately followed by an insertion is one word standing in for another,
            // which is what a mispronounced name looks like after recognition.
            let substituted =
                lengths[(row + 1) * columns + column + 1] == lengths[(row + 1) * columns + column];
            if substituted {
                differences.push(WordDifference::Replaced {
                    expected: expected_words[row].original.clone(),
                    heard: heard_words[column].original.clone(),
                    at_index: row,
                });
                row += 1;
                column += 1;
            } else {
                differences.push(WordDifference::Skipped {
                    expected: expected_words[row].original.clone(),
                    at_index: row,
                });
                row += 1;
            }
        } else {
            differences.push(WordDifference::Inserted {
                heard: heard_words[column].original.clone(),
                at_index: row,
            });
            column += 1;
        }
    }
    while row < expected_words.len() {
        differences.push(WordDifference::Skipped {
            expected: expected_words[row].original.clone(),
            at_index: row,
        });
        row += 1;
    }
    while column < heard_words.len() {
        differences.push(WordDifference::Inserted {
            heard: heard_words[column].original.clone(),
            at_index: expected_words.len(),
        });
        column += 1;
    }
    differences
}

struct SpokenWord {
    original: String,
    normalized: String,
}

/// Merge adjacent words in `words` wherever their concatenation is a word in `against`.
fn join_compounds(words: Vec<SpokenWord>, against: &[SpokenWord]) -> Vec<SpokenWord> {
    let vocabulary = against
        .iter()
        .map(|word| word.normalized.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut joined = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        if index + 1 < words.len() {
            let compound = format!("{}{}", words[index].normalized, words[index + 1].normalized);
            if vocabulary.contains(compound.as_str()) {
                joined.push(SpokenWord {
                    original: format!("{} {}", words[index].original, words[index + 1].original),
                    normalized: compound,
                });
                index += 2;
                continue;
            }
        }
        joined.push(SpokenWord {
            original: words[index].original.clone(),
            normalized: words[index].normalized.clone(),
        });
        index += 1;
    }
    joined
}

fn normalize_words(text: &str) -> Vec<SpokenWord> {
    text.split_whitespace()
        .filter_map(|word| {
            let normalized = word
                .chars()
                .filter(|character| character.is_alphanumeric() || *character == '\'')
                .collect::<String>()
                .to_lowercase();
            if normalized.is_empty() {
                return None;
            }
            Some(SpokenWord {
                original: word.to_string(),
                normalized: spell_number(&normalized),
            })
        })
        .collect()
}

/// A recogniser writes "twenty" as "20"; the script wrote a word. Both are the same thing said,
/// so digits up to ninety-nine are compared as the words a voice would say.
fn spell_number(token: &str) -> String {
    let Ok(value) = token.parse::<u32>() else {
        return token.to_string();
    };
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    match value {
        0..=19 => ONES[value as usize].to_string(),
        20..=99 if value % 10 == 0 => TENS[(value / 10) as usize].to_string(),
        20..=99 => format!(
            "{}{}",
            TENS[(value / 10) as usize],
            ONES[(value % 10) as usize]
        ),
        _ => token.to_string(),
    }
}

/// Compare what a performed line was asked to do with what a recogniser heard.
///
/// A recogniser writes a laugh down as "ha ha ha" and a sigh as "ah", so those are not inserted
/// words when the line asked for the event. The one thing it must never hear is the cue's own
/// name: "laughter" in a take that was meant to laugh means the voice read the cue as text, and
/// that is reported as its own finding rather than buried among ordinary insertions.
pub fn findings_for_performed_line(turn_id: &str, asked: &str, heard: &str) -> Vec<QcFinding> {
    let events = super::vocal_events::events_of(asked);
    let words = super::vocal_events::words_of(asked);
    if events.is_empty() {
        return findings_for_turn(turn_id, &diff_spoken_words(&words, heard));
    }
    let expected = normalize_words(&words)
        .into_iter()
        .map(|word| word.normalized)
        .collect::<std::collections::BTreeSet<_>>();
    let performed = events
        .iter()
        .flat_map(|event| event.heard_as().iter().copied())
        .collect::<std::collections::BTreeSet<_>>();
    let misread = events
        .iter()
        .flat_map(|event| event.misread_as().iter().map(move |word| (*word, *event)))
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut findings = Vec::new();
    let mut remaining = Vec::new();
    for word in normalize_words(heard) {
        if let Some(event) = misread.get(word.normalized.as_str()) {
            if !expected.contains(&word.normalized) {
                findings.push(QcFinding {
                    id: format!("qc-{turn_id}-cue-{}", findings.len()),
                    kind: QcFindingKind::SpokenCue,
                    // The audience said "laughter". Nothing about that is publishable.
                    severity: QcSeverity::Blocking,
                    turn_id: Some(turn_id.to_string()),
                    detail: format!(
                        "The take says \"{}\" where the script asked the voice to {}",
                        word.original,
                        event.canonical()
                    ),
                    at_us: None,
                });
                continue;
            }
        }
        if performed.contains(word.normalized.as_str()) && !expected.contains(&word.normalized) {
            continue;
        }
        remaining.push(word.original);
    }
    if words.is_empty() {
        // A reaction has no words to diff. Whatever else a recogniser made of a laugh is noise,
        // not a misstatement.
        return findings;
    }
    findings.extend(findings_for_turn(
        turn_id,
        &diff_spoken_words(&words, &remaining.join(" ")),
    ));
    findings
}

/// Report cues a take was asked for and could not perform.
pub fn findings_for_dropped_cues(
    turn_id: &str,
    dropped: &[super::vocal_events::VocalEvent],
) -> Vec<QcFinding> {
    dropped
        .iter()
        .enumerate()
        .map(|(index, event)| QcFinding {
            id: format!("qc-{turn_id}-dropped-{index}"),
            kind: QcFindingKind::DroppedCue,
            // The line still says what it should; it just does not laugh.
            severity: QcSeverity::Notice,
            turn_id: Some(turn_id.to_string()),
            detail: format!(
                "The cast voice cannot {}; the cue was removed rather than spoken",
                event.canonical()
            ),
            at_us: None,
        })
        .collect()
}

/// Report a performed episode that missed the length its show asked for.
pub fn findings_for_length(
    target: &super::contracts::LengthTarget,
    actual_us: Microseconds,
) -> Vec<QcFinding> {
    if target.accepts(actual_us) {
        return Vec::new();
    }
    let delta = target.delta_us(actual_us);
    vec![QcFinding {
        id: "qc-length".to_string(),
        kind: QcFindingKind::DurationOffTarget,
        // A thirty-second spot that runs a minute is a different deliverable, not a long one.
        severity: QcSeverity::Blocking,
        turn_id: None,
        detail: format!(
            "The episode runs {:.1}s against a target of {:.1}s (allowed within {:.1}s); it is {:.1}s {}",
            (actual_us.0 as f64) / 1_000_000.0,
            (target.target_us.0 as f64) / 1_000_000.0,
            (target.tolerance_us().0 as f64) / 1_000_000.0,
            (delta.0.abs() as f64) / 1_000_000.0,
            if delta.0 > 0 { "too long" } else { "too short" }
        ),
        at_us: None,
    }]
}

/// A quality check as it was recorded, bound to the episode it checked.
///
/// The fingerprint covers everything the check listened to: the takes, the master, the mix
/// targets, and the performed length. A later change to any of them makes the record stale, and a
/// stale record is reported as no record, because a check of a different episode proves nothing
/// about this one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualityRecord {
    pub project_id: String,
    pub version_id: String,
    pub revision: u64,
    pub fingerprint: String,
    pub checked_at: String,
    pub report: QcReport,
}

/// What a release planner needs to know about the last quality check.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum QualityStatus {
    /// No check has been recorded for this episode.
    Unchecked,
    /// A check exists, but the episode changed after it.
    Stale,
    /// The last check stands and found blocking problems.
    Blocking {
        findings: usize,
        unchecked_turns: usize,
    },
    /// The last check stands, found nothing blocking, but did not measure the master's loudness.
    Unmeasured,
    /// The last check stands and found nothing blocking.
    Clear,
}

impl QualityRecord {
    /// How this record relates to the episode as it now stands.
    pub fn status_for(&self, manifest: &super::contracts::VideoProjectManifest) -> QualityStatus {
        if self.fingerprint != quality_fingerprint(manifest) {
            return QualityStatus::Stale;
        }
        if self.report.is_clear() {
            QualityStatus::Clear
        } else if self.report.blocking().is_empty()
            && self.report.unchecked_turns.is_empty()
            && !self.report.loudness_checked
        {
            QualityStatus::Unmeasured
        } else {
            QualityStatus::Blocking {
                findings: self.report.blocking().len(),
                unchecked_turns: self.report.unchecked_turns.len(),
            }
        }
    }
}

/// Everything a quality check listened to, as one hash.
pub fn quality_fingerprint(manifest: &super::contracts::VideoProjectManifest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"quality-v1");
    let mut bindings = manifest
        .narration_bindings
        .iter()
        .map(|binding| {
            format!(
                "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{:?}",
                binding.turn_id.as_deref().unwrap_or_default(),
                binding.render_artifact_id,
                binding.script_sha256,
                binding
                    .performance
                    .as_ref()
                    .map(|performance| performance.fingerprint.as_str())
                    .unwrap_or_default(),
                binding.fidelity
            )
        })
        .collect::<Vec<_>>();
    bindings.sort();
    for binding in bindings {
        hasher.update([0x1f]);
        hasher.update(binding.as_bytes());
    }
    let mut masters = manifest
        .render_artifacts
        .iter()
        .filter(|artifact| {
            matches!(
                artifact.role,
                super::contracts::RenderArtifactRole::FinalMaster
            ) && matches!(
                artifact.publication_state,
                super::contracts::PublicationState::Published
            )
        })
        .map(|artifact| artifact.sha256.clone())
        .collect::<Vec<_>>();
    masters.sort();
    for master in masters {
        hasher.update([0x1e]);
        hasher.update(master.as_bytes());
    }
    hasher.update([0x1d]);
    hasher.update(manifest.timeline_duration_us.0.to_string().as_bytes());
    hasher.update([0x1d]);
    hasher.update(manifest.audio_mix.target_lufs_milli.to_string().as_bytes());
    hasher.update([0x1d]);
    hasher.update(manifest.audio_mix.true_peak_db_milli.to_string().as_bytes());
    if let Some(target) = &manifest.length_target {
        hasher.update([0x1d]);
        hasher.update(target.target_us.0.to_string().as_bytes());
        hasher.update(target.tolerance_bp.to_string().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Turn one take's word differences into reviewable findings.
pub fn findings_for_turn(turn_id: &str, differences: &[WordDifference]) -> Vec<QcFinding> {
    differences
        .iter()
        .enumerate()
        .map(|(index, difference)| {
            let (kind, detail) = match difference {
                WordDifference::Skipped { expected, .. } => (
                    QcFindingKind::SkippedWord,
                    format!("The take does not say \"{expected}\""),
                ),
                WordDifference::Inserted { heard, .. } => (
                    QcFindingKind::InsertedWord,
                    format!("The take says \"{heard}\", which the script does not contain"),
                ),
                WordDifference::Replaced {
                    expected, heard, ..
                } => (
                    QcFindingKind::ReplacedWord,
                    format!("The take says \"{heard}\" where the script says \"{expected}\""),
                ),
            };
            QcFinding {
                id: format!("qc-{turn_id}-{index:03}"),
                kind,
                // The episode not saying what it was asked to say is the one thing that cannot be
                // waved through, whichever shape it takes.
                severity: QcSeverity::Blocking,
                turn_id: Some(turn_id.to_string()),
                detail,
                at_us: None,
            }
        })
        .collect()
}

/// Compare a measured master against the targets the format asked for.
pub fn findings_for_loudness(
    measured: LoudnessMeasurement,
    target_lufs_milli: i32,
    true_peak_ceiling_milli: i32,
) -> Vec<QcFinding> {
    let mut findings = Vec::new();
    let drift = measured.integrated_lufs_milli - target_lufs_milli;
    if drift.abs() > LOUDNESS_TOLERANCE_MILLI {
        findings.push(QcFinding {
            id: "qc-loudness".to_string(),
            kind: QcFindingKind::LoudnessOffTarget,
            // A platform will normalize an off-target master, changing how the episode sounds
            // after it leaves soundAr.
            severity: QcSeverity::Blocking,
            turn_id: None,
            detail: format!(
                "The master measures {:.1} LUFS against a target of {:.1}",
                f64::from(measured.integrated_lufs_milli) / 1000.0,
                f64::from(target_lufs_milli) / 1000.0
            ),
            at_us: None,
        });
    }
    if measured.true_peak_db_milli > true_peak_ceiling_milli + TRUE_PEAK_TOLERANCE_MILLI {
        findings.push(QcFinding {
            id: "qc-true-peak".to_string(),
            kind: QcFindingKind::TruePeakExceeded,
            severity: QcSeverity::Blocking,
            turn_id: None,
            detail: format!(
                "The master peaks at {:.1} dBTP against a ceiling of {:.1}",
                f64::from(measured.true_peak_db_milli) / 1000.0,
                f64::from(true_peak_ceiling_milli) / 1000.0
            ),
            at_us: None,
        });
    }
    findings
}

/// One caption and the word timing it is supposed to sit on.
#[derive(Clone, Copy, Debug)]
pub struct CaptionAlignment<'a> {
    pub caption_id: &'a str,
    pub caption_start_us: Microseconds,
    pub spoken_start_us: Microseconds,
}

/// Report captions that have drifted away from the words they belong to.
pub fn findings_for_caption_drift(alignments: &[CaptionAlignment<'_>]) -> Vec<QcFinding> {
    alignments
        .iter()
        .filter_map(|alignment| {
            let drift = alignment.caption_start_us.0 - alignment.spoken_start_us.0;
            if drift.abs() <= CAPTION_DRIFT_TOLERANCE_US {
                return None;
            }
            Some(QcFinding {
                id: format!("qc-caption-{}", alignment.caption_id),
                kind: QcFindingKind::CaptionDrift,
                // Visible but not a misstatement: the episode still says what it should.
                severity: QcSeverity::Warning,
                turn_id: None,
                detail: format!(
                    "This caption appears {:.2}s {} the words it belongs to",
                    (drift.abs() as f64) / 1_000_000.0,
                    if drift > 0 { "after" } else { "before" }
                ),
                at_us: Some(alignment.caption_start_us),
            })
        })
        .collect()
}

/// Report silence long enough to read as a fault.
///
/// `spans` are the occupied ranges of the speech track, in timeline order. Gaps between them are
/// the silence a listener actually hears.
pub fn findings_for_dead_air(
    spans: &[(Microseconds, Microseconds)],
    timeline_duration_us: Microseconds,
) -> Vec<QcFinding> {
    let mut findings = Vec::new();
    let mut cursor = Microseconds::ZERO;
    let mut ordered = spans.to_vec();
    ordered.sort_by_key(|(start, end)| (*start, *end));
    for (start, end) in ordered.iter().chain(std::iter::once(&(
        timeline_duration_us,
        timeline_duration_us,
    ))) {
        let gap = start.0 - cursor.0;
        if gap > DEAD_AIR_THRESHOLD_US {
            findings.push(QcFinding {
                id: format!("qc-dead-air-{:012}", cursor.0),
                kind: QcFindingKind::DeadAir,
                severity: QcSeverity::Warning,
                turn_id: None,
                detail: format!("{:.1}s of silence", (gap as f64) / 1_000_000.0),
                at_us: Some(cursor),
            });
        }
        cursor = cursor.max(*end);
    }
    findings
}

/// Read a loudness measurement out of FFmpeg's `loudnorm` analysis output.
///
/// `loudnorm` prints a JSON object to stderr in analysis mode. It is parsed rather than recomputed
/// because these are the numbers a platform's own normalizer will read, and approximating them here
/// would report a different episode than the one that ships.
///
/// Returns `None` when the output carries no usable measurement. An unparsed run is reported as
/// unmeasured rather than as a default, which is the difference between "not checked" and "fine".
pub fn parse_loudness_analysis(output: &str) -> Option<LoudnessMeasurement> {
    // The JSON block is the last one printed; earlier braces belong to FFmpeg's own diagnostics.
    let start = output.rfind('{')?;
    let end = output[start..].find('}')? + start + 1;
    let parsed: serde_json::Value = serde_json::from_str(&output[start..end]).ok()?;

    let read = |key: &str| -> Option<i32> {
        let raw = parsed.get(key)?.as_str()?;
        // `loudnorm` reports `-inf` for silence, which is a real answer but not a number this
        // contract can carry, so it is treated as unmeasured rather than clamped to a value.
        let value = raw.parse::<f64>().ok()?;
        if !value.is_finite() {
            return None;
        }
        Some((value * 1000.0).round() as i32)
    };
    Some(LoudnessMeasurement {
        integrated_lufs_milli: read("input_i")?,
        true_peak_db_milli: read("input_tp")?,
    })
}

/// Assemble one report. Findings are ordered by severity so the worst is read first.
pub fn build_report(
    mut findings: Vec<QcFinding>,
    checked_turns: Vec<String>,
    unchecked_turns: Vec<String>,
    loudness_checked: bool,
) -> VideoResult<QcReport> {
    for finding in &findings {
        finding.validate()?;
    }
    findings.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(QcReport {
        findings,
        checked_turns,
        unchecked_turns,
        loudness_checked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compound_is_the_same_words_said() {
        assert!(diff_spoken_words(
            "I'm the one paying. Good night!",
            "I'm the one paying, goodnight."
        )
        .is_empty());
        assert!(diff_spoken_words("my smartphone rang", "my smart phone rang").is_empty());
        assert_eq!(diff_spoken_words("Good night", "good fight").len(), 1);
    }

    #[test]
    fn digits_and_number_words_are_the_same_thing_said() {
        assert!(diff_spoken_words("for twenty minutes", "for 20 minutes").is_empty());
        assert!(diff_spoken_words("twenty-five past", "25 past").is_empty());
        assert_eq!(
            diff_spoken_words("for twenty minutes", "for 21 minutes").len(),
            1
        );
    }

    #[test]
    fn a_true_peak_a_hair_over_the_ceiling_is_the_meter_not_the_mix() {
        let measured = LoudnessMeasurement {
            integrated_lufs_milli: -16_000,
            true_peak_db_milli: -1_400,
        };
        assert!(findings_for_loudness(measured, -16_000, -1_500).is_empty());
        let hot = LoudnessMeasurement {
            integrated_lufs_milli: -16_000,
            true_peak_db_milli: -900,
        };
        assert_eq!(findings_for_loudness(hot, -16_000, -1_500).len(), 1);
    }

    #[test]
    fn a_performed_laugh_is_not_an_inserted_word_but_a_read_one_is() {
        let asked = "(laugh) Good evening! I bought a smart refrigerator.";
        let performed = findings_for_performed_line(
            "turn-1",
            asked,
            "Ha ha ha! Good evening! I bought a smart refrigerator.",
        );
        assert!(performed.is_empty(), "{performed:?}");

        let read = findings_for_performed_line(
            "turn-1",
            asked,
            "Laughs. Good evening! I bought a smart refrigerator.",
        );
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].kind, QcFindingKind::SpokenCue);
        assert_eq!(read[0].severity, QcSeverity::Blocking);
        assert!(read[0].detail.contains("laugh"), "{}", read[0].detail);
    }

    #[test]
    fn a_reaction_is_judged_only_on_whether_its_cue_was_read() {
        assert!(
            findings_for_performed_line("turn-2", "(laugh) (applause)", "Ha ha ha! Yeah!")
                .is_empty()
        );
        let read =
            findings_for_performed_line("turn-2", "(laugh) (applause)", "Laughter. Applause.");
        assert_eq!(read.len(), 2);
        assert!(read
            .iter()
            .all(|finding| finding.kind == QcFindingKind::SpokenCue));
    }

    #[test]
    fn a_line_without_cues_is_diffed_word_for_word_as_before() {
        let findings = findings_for_performed_line("turn-3", "Same, fridge.", "Same fridge.");
        assert!(findings.is_empty());
        let findings = findings_for_performed_line("turn-3", "Same, fridge.", "Same bridge.");
        assert_eq!(findings[0].kind, QcFindingKind::ReplacedWord);
    }

    #[test]
    fn a_dropped_cue_is_a_notice_and_a_missed_length_is_blocking() {
        let dropped =
            findings_for_dropped_cues("turn-4", &[super::super::vocal_events::VocalEvent::Sigh]);
        assert_eq!(dropped[0].kind, QcFindingKind::DroppedCue);
        assert_eq!(dropped[0].severity, QcSeverity::Notice);

        let target = super::super::contracts::LengthTarget {
            target_us: Microseconds(30_000_000),
            tolerance_bp: 2_000,
        };
        assert!(findings_for_length(&target, Microseconds(35_000_000)).is_empty());
        let long = findings_for_length(&target, Microseconds(63_000_000));
        assert_eq!(long[0].kind, QcFindingKind::DurationOffTarget);
        assert_eq!(long[0].severity, QcSeverity::Blocking);
        assert!(
            long[0].detail.contains("33.0s too long"),
            "{}",
            long[0].detail
        );
    }

    #[test]
    fn punctuation_and_capitalisation_are_not_errors() {
        // A recognizer does not reproduce these, and reporting them would bury the real findings.
        assert!(diff_spoken_words(
            "The harmattan came early, and she waited.",
            "the harmattan came early and she waited"
        )
        .is_empty());
    }

    #[test]
    fn a_skipped_word_is_reported_with_the_word_that_is_missing() {
        let differences = diff_spoken_words("She said nothing at all.", "She said nothing at all");
        assert!(differences.is_empty());

        let differences = diff_spoken_words("She said nothing at all.", "She said nothing all");
        assert_eq!(
            differences,
            vec![WordDifference::Skipped {
                expected: "at".into(),
                at_index: 3
            }]
        );
    }

    #[test]
    fn an_inserted_word_is_reported() {
        let differences = diff_spoken_words("She waited.", "She simply waited");
        assert_eq!(
            differences,
            vec![WordDifference::Inserted {
                heard: "simply".into(),
                at_index: 1
            }]
        );
    }

    #[test]
    fn a_mispronounced_name_reads_as_a_replacement_rather_than_two_errors() {
        // This is the shape a mispronounced invented name takes after recognition.
        let differences = diff_spoken_words("Adaeze came home.", "Adaze came home");
        assert_eq!(
            differences,
            vec![WordDifference::Replaced {
                expected: "Adaeze".into(),
                heard: "Adaze".into(),
                at_index: 0
            }]
        );
    }

    #[test]
    fn an_empty_take_reports_every_scripted_word_as_skipped() {
        let differences = diff_spoken_words("Two words", "");
        assert_eq!(differences.len(), 2);
        assert!(differences
            .iter()
            .all(|difference| matches!(difference, WordDifference::Skipped { .. })));
    }

    #[test]
    fn every_word_difference_blocks_because_the_episode_misstates_its_script() {
        let differences = diff_spoken_words("Adaeze came home.", "Adaze came");
        let findings = findings_for_turn("turn-a", &differences);
        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .all(|finding| matches!(finding.severity, QcSeverity::Blocking)));
        assert!(findings
            .iter()
            .all(|finding| finding.turn_id.as_deref() == Some("turn-a")));
        // The detail is written in the user's own words, not as an opaque code.
        assert!(
            findings[0].detail.contains("Adaze"),
            "{}",
            findings[0].detail
        );
    }

    #[test]
    fn a_loudness_analysis_is_read_rather_than_recomputed() {
        // The shape `loudnorm` actually prints in analysis mode.
        let output = r#"[Parsed_loudnorm_0 @ 0x55] 
{
	"input_i" : "-18.42",
	"input_tp" : "-1.75",
	"input_lra" : "7.20",
	"input_thresh" : "-28.61",
	"output_i" : "-16.00"
}
"#;
        let measured = parse_loudness_analysis(output).expect("a measurement");
        assert_eq!(measured.integrated_lufs_milli, -18_420);
        assert_eq!(measured.true_peak_db_milli, -1_750);
    }

    #[test]
    fn silence_is_reported_as_unmeasured_rather_than_as_a_number() {
        // `loudnorm` reports -inf for silence. That is a real answer, but not one this contract
        // can carry, and clamping it would report a loudness the episode does not have.
        let output = r#"{"input_i" : "-inf", "input_tp" : "-inf"}"#;
        assert_eq!(parse_loudness_analysis(output), None);
    }

    #[test]
    fn an_unreadable_analysis_is_unmeasured_rather_than_defaulted() {
        assert_eq!(parse_loudness_analysis(""), None);
        assert_eq!(parse_loudness_analysis("ffmpeg failed"), None);
        assert_eq!(
            parse_loudness_analysis(r#"{"input_i" : "not a number"}"#),
            None
        );
        // A measurement missing its true peak is only half a check.
        assert_eq!(parse_loudness_analysis(r#"{"input_i" : "-18.0"}"#), None);
    }

    #[test]
    fn loudness_within_tolerance_is_not_reported() {
        let measurement = LoudnessMeasurement {
            integrated_lufs_milli: -16_400,
            true_peak_db_milli: -1_500,
        };
        assert!(findings_for_loudness(measurement, -16_000, -1_000).is_empty());
    }

    #[test]
    fn an_off_target_master_blocks_because_a_platform_will_change_how_it_sounds() {
        let measurement = LoudnessMeasurement {
            integrated_lufs_milli: -21_000,
            true_peak_db_milli: -500,
        };
        let findings = findings_for_loudness(measurement, -16_000, -1_000);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].kind, QcFindingKind::LoudnessOffTarget);
        assert_eq!(findings[1].kind, QcFindingKind::TruePeakExceeded);
        assert!(findings
            .iter()
            .all(|finding| matches!(finding.severity, QcSeverity::Blocking)));
        assert!(
            findings[0].detail.contains("-21.0"),
            "{}",
            findings[0].detail
        );
    }

    #[test]
    fn caption_drift_is_reported_with_its_direction() {
        let aligned = CaptionAlignment {
            caption_id: "caption-1",
            caption_start_us: Microseconds(1_000_000),
            spoken_start_us: Microseconds(1_100_000),
        };
        assert!(findings_for_caption_drift(&[aligned]).is_empty());

        let late = CaptionAlignment {
            caption_id: "caption-2",
            caption_start_us: Microseconds(2_000_000),
            spoken_start_us: Microseconds(1_000_000),
        };
        let findings = findings_for_caption_drift(&[late]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, QcFindingKind::CaptionDrift);
        assert!(
            findings[0].detail.contains("after"),
            "{}",
            findings[0].detail
        );
        assert_eq!(findings[0].at_us, Some(Microseconds(2_000_000)));
    }

    #[test]
    fn dead_air_is_reported_but_an_ordinary_beat_is_not() {
        let spans = vec![
            (Microseconds(0), Microseconds(5_000_000)),
            // A one-second beat between lines is a performance choice, not a fault.
            (Microseconds(6_000_000), Microseconds(10_000_000)),
            // Eight seconds of nothing is a fault.
            (Microseconds(18_000_000), Microseconds(20_000_000)),
        ];
        let findings = findings_for_dead_air(&spans, Microseconds(20_000_000));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].at_us, Some(Microseconds(10_000_000)));
        assert!(findings[0].detail.contains("8.0"), "{}", findings[0].detail);
    }

    #[test]
    fn silence_at_the_end_of_an_episode_is_reported_too() {
        let spans = vec![(Microseconds(0), Microseconds(5_000_000))];
        let findings = findings_for_dead_air(&spans, Microseconds(20_000_000));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].at_us, Some(Microseconds(5_000_000)));
    }

    #[test]
    fn a_turn_nobody_listened_back_to_is_not_a_turn_that_passed() {
        let clear = build_report(Vec::new(), vec!["turn-a".into()], Vec::new(), true).unwrap();
        assert!(clear.is_clear());

        let unchecked = build_report(
            Vec::new(),
            vec!["turn-a".into()],
            vec!["turn-b".into()],
            true,
        )
        .unwrap();
        assert!(!unchecked.is_clear());

        // Without a measurement the report cannot claim the master is within its target.
        let unmeasured =
            build_report(Vec::new(), vec!["turn-a".into()], Vec::new(), false).unwrap();
        assert!(!unmeasured.is_clear());
    }

    #[test]
    fn the_worst_finding_is_read_first() {
        let findings = vec![
            QcFinding {
                id: "qc-caption-1".into(),
                kind: QcFindingKind::CaptionDrift,
                severity: QcSeverity::Warning,
                turn_id: None,
                detail: "drifted".into(),
                at_us: None,
            },
            QcFinding {
                id: "qc-turn-a-000".into(),
                kind: QcFindingKind::SkippedWord,
                severity: QcSeverity::Blocking,
                turn_id: Some("turn-a".into()),
                detail: "missing".into(),
                at_us: None,
            },
        ];
        let report = build_report(findings, vec!["turn-a".into()], Vec::new(), true).unwrap();
        assert_eq!(report.findings[0].severity, QcSeverity::Blocking);
        assert!(!report.is_clear());
        assert_eq!(report.blocking().len(), 1);
    }
}
