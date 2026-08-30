//! Music as score rather than attachment.
//!
//! "Put music at the end" is really four different jobs: a sting that opens, a bed that sits under
//! speech, a transition that covers a cut, and an outro that resolves after the last line. Each has
//! a different relationship to the timeline and to the voices, and treating them as one generic
//! audio file is why generated episodes usually sound like a podcast with a song stapled to it.
//!
//! A `MusicCue` names that relationship. The cue's role decides where it sits, how it is mixed, and
//! how long it has to run; the writer supplies direction and the local music engine supplies the
//! audio. A `Bed` is bound to a ducking envelope sidechained to the speech track automatically,
//! because a bed that does not duck is the single most common way a mix buries its own dialogue.
//!
//! Durations here are targets, not measurements. A cue that has not been rendered yet reports the
//! length it was asked for and nothing else; the fitted length only becomes real once local
//! generation has produced audio.

use super::contracts::{
    validate_identifier, validate_nonempty, validate_timestamp_text, Microseconds, Validate,
    VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_MUSIC_CUES: usize = 64;
pub const MAX_CUE_DURATION_US: i64 = 15 * 60 * 1_000_000;
pub const MIN_CUE_DURATION_US: i64 = 500_000;
pub const MAX_DIRECTION_BYTES: usize = 2_000;

/// How far a fitted cue may fall short of, or run past, its target before the fit is a failure
/// rather than a trim. A generated piece that lands within a second of its target can be faded to
/// length musically; one that lands four seconds short leaves audible silence where the score
/// should be.
pub const CUE_FIT_TOLERANCE_US: i64 = 1_500_000;

/// The default reduction a bed takes while a voice is speaking over it.
pub const DEFAULT_BED_DUCK_DB_MILLI: i32 = -12_000;
pub const DEFAULT_DUCK_ATTACK_US: i64 = 120_000;
pub const DEFAULT_DUCK_RELEASE_US: i64 = 450_000;

/// What a cue is for. The role, not the audio, decides how it is placed and mixed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CueRole {
    /// A short opening figure. Plays at full level; nothing speaks over it.
    Sting,
    /// Continuous music under dialogue. Always ducked against speech.
    Bed,
    /// Covers a cut between scenes.
    Transition,
    /// Resolves after the final line. This is what ends the episode.
    Outro,
}

impl CueRole {
    /// Whether this role plays underneath speech and must therefore duck.
    pub const fn is_underscore(self) -> bool {
        matches!(self, Self::Bed)
    }
}

/// Where a cue sits on the timeline.
///
/// Anchoring to a scene or turn rather than to an absolute timestamp is what lets a cue survive
/// editing: re-reading a line or retiming a pause moves the anchor, and the cue moves with it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CueAnchor {
    Scene {
        scene_id: String,
    },
    Turn {
        turn_id: String,
    },
    /// After the final line. The only anchor an `Outro` may use, and the only role that may use it.
    AfterFinalTurn,
}

/// One piece of score.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MusicCue {
    pub id: String,
    pub role: CueRole,
    pub anchor: CueAnchor,
    /// How long the cue should run. The local music engine is asked for this length and the
    /// rendered result is fitted to it.
    pub target_duration_us: Microseconds,
    /// The written direction handed to the music engine.
    pub direction: String,
    /// The registered soundAr music artifact once it exists. `None` means this cue is planned but
    /// not yet generated, and no length has been measured for it.
    #[serde(default)]
    pub source_asset_id: Option<String>,
    /// The timeline audio track carrying this cue, once its music has been placed. A `Bed` with a
    /// track is required to duck; that is the whole reason the association is recorded here rather
    /// than left to a naming convention the renderer would have to guess at.
    #[serde(default)]
    pub track_id: Option<String>,
    pub gain_db_milli: i32,
    pub fade_in_us: Microseconds,
    pub fade_out_us: Microseconds,
    pub created_at: String,
}

impl MusicCue {
    /// Whether this cue still needs local music generation.
    pub fn needs_generation(&self) -> bool {
        self.source_asset_id.is_none()
    }
}

impl Validate for MusicCue {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "music_cues.id")?;
        validate_nonempty(&self.direction, "music_cues.direction", MAX_DIRECTION_BYTES)?;
        if !(MIN_CUE_DURATION_US..=MAX_CUE_DURATION_US).contains(&self.target_duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCue,
                "a cue must run between half a second and fifteen minutes",
            )
            .at("music_cues.target_duration_us"));
        }
        if !(-60_000..=12_000).contains(&self.gain_db_milli) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCue,
                "cue gain is outside the supported range",
            )
            .at("music_cues.gain_db_milli"));
        }
        if self.fade_in_us.0 < 0 || self.fade_out_us.0 < 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCue,
                "cue fades cannot be negative",
            )
            .at("music_cues.fade_in_us"));
        }
        // Fades that overlap would be applied to the same audio twice, and the renderer would have
        // to invent which one wins.
        if self
            .fade_in_us
            .0
            .checked_add(self.fade_out_us.0)
            .is_none_or(|total| total > self.target_duration_us.0)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCue,
                "cue fades cannot together exceed the cue's own length",
            )
            .at("music_cues.fade_out_us"));
        }
        match (&self.anchor, self.role) {
            (CueAnchor::AfterFinalTurn, CueRole::Outro) => {}
            (CueAnchor::AfterFinalTurn, _) => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidCue,
                    "only an outro may play after the final line",
                )
                .at("music_cues.anchor"));
            }
            (_, CueRole::Outro) => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidCue,
                    "an outro must be anchored after the final line",
                )
                .at("music_cues.anchor"));
            }
            (CueAnchor::Scene { scene_id }, _) => {
                validate_identifier(scene_id, "music_cues.anchor.scene_id")?;
            }
            (CueAnchor::Turn { turn_id }, _) => {
                validate_identifier(turn_id, "music_cues.anchor.turn_id")?;
            }
        }
        if let Some(asset_id) = &self.source_asset_id {
            validate_identifier(asset_id, "music_cues.source_asset_id")?;
        }
        if let Some(track_id) = &self.track_id {
            validate_identifier(track_id, "music_cues.track_id")?;
        }
        // A track without audio behind it would place silence on the timeline and report it as
        // score.
        if self.track_id.is_some() && self.source_asset_id.is_none() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCue,
                "a cue cannot occupy a timeline track before its music exists",
            )
            .at("music_cues.track_id"));
        }
        validate_timestamp_text(&self.created_at, "music_cues.created_at")?;
        Ok(())
    }
}

/// The ducking envelope a bed must carry.
///
/// This is derived rather than authored so a bed cannot be added without one. The reduction is
/// deliberately generous: a bed that is merely quieter than the voice still competes with it.
pub fn bed_ducking(speech_track_id: &str) -> super::contracts::DuckingSpec {
    super::contracts::DuckingSpec {
        sidechain_track_id: speech_track_id.to_string(),
        reduction_db_milli: DEFAULT_BED_DUCK_DB_MILLI,
        attack_us: Microseconds(DEFAULT_DUCK_ATTACK_US),
        release_us: Microseconds(DEFAULT_DUCK_RELEASE_US),
    }
}

/// How a generated piece was made to land on its target length.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CueFitAction {
    /// The generated length already matched within tolerance.
    Exact,
    /// The piece ran long and was trimmed, with the cue's fade-out carrying the tail.
    TrimmedWithTail,
    /// The piece ran short within tolerance and is held to length by its fade.
    HeldByFade,
}

/// The plan for making one generated piece land on its cue's target.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CueFit {
    pub action: CueFitAction,
    /// Where in the generated audio the cue starts using it. Always zero today; present so a later
    /// revision can lift a section out of a longer piece without changing this contract.
    pub source_start_us: Microseconds,
    pub source_end_us: Microseconds,
    /// The fade-out actually applied, which may be longer than the cue's own fade when a trim
    /// needs to carry a musical tail rather than cutting the piece off.
    pub fade_out_us: Microseconds,
}

/// Fit generated music to its cue.
///
/// A hard cut at the target length is what makes generated score sound amateur, so a piece that
/// runs long is trimmed with its fade-out extended to carry a tail. A piece that falls more than
/// the tolerance short cannot be stretched honestly - stretching would change its tempo - so it is
/// reported as a failure for regeneration rather than padded with silence.
pub fn fit_cue(cue: &MusicCue, generated_duration_us: Microseconds) -> VideoResult<CueFit> {
    if generated_duration_us.0 <= 0 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCue,
            "generated music has no measured duration to fit",
        )
        .at("music_cues.source_asset_id"));
    }
    let target = cue.target_duration_us.0;
    let generated = generated_duration_us.0;
    let shortfall = target - generated;

    if shortfall > CUE_FIT_TOLERANCE_US {
        return Err(VideoError::new(
            VideoErrorCode::CueFitFailed,
            "the generated music is too short for this cue; regenerate it at the cue's length",
        )
        .at("music_cues.target_duration_us"));
    }
    if shortfall > 0 {
        // Short but within tolerance: the fade holds the ending rather than leaving dead air.
        return Ok(CueFit {
            action: CueFitAction::HeldByFade,
            source_start_us: Microseconds::ZERO,
            source_end_us: generated_duration_us,
            fade_out_us: cue.fade_out_us,
        });
    }
    if -shortfall <= CUE_FIT_TOLERANCE_US && cue.fade_out_us.0 > 0 {
        return Ok(CueFit {
            action: CueFitAction::Exact,
            source_start_us: Microseconds::ZERO,
            source_end_us: Microseconds(target),
            fade_out_us: cue.fade_out_us,
        });
    }
    // Long enough that the trim is audible unless a tail carries it. The tail is bounded by the
    // cue's own length so a short sting cannot end up almost entirely fade.
    let tail = cue
        .fade_out_us
        .0
        .max(DEFAULT_DUCK_RELEASE_US * 2)
        .min(target / 2);
    Ok(CueFit {
        action: CueFitAction::TrimmedWithTail,
        source_start_us: Microseconds::ZERO,
        source_end_us: Microseconds(target),
        fade_out_us: Microseconds(tail),
    })
}

/// Validate a project's cue sheet against the scenes, turns, and assets it references.
pub(crate) fn validate_cue_sheet(
    cues: &[MusicCue],
    scene_ids: &BTreeSet<&str>,
    turn_ids: &BTreeSet<&str>,
    music_asset_ids: &BTreeSet<&str>,
    has_dialogue: bool,
) -> VideoResult<()> {
    if cues.len() > MAX_MUSIC_CUES {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCue,
            format!("a project supports at most {MAX_MUSIC_CUES} music cues"),
        )
        .at("music_cues"));
    }
    let mut seen_ids = BTreeSet::new();
    let mut outros = 0usize;
    for cue in cues {
        cue.validate()?;
        if !seen_ids.insert(cue.id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::DuplicateId,
                format!("duplicate identifier {}", cue.id),
            )
            .at("music_cues.id"));
        }
        match &cue.anchor {
            CueAnchor::Scene { scene_id } => {
                if !scene_ids.contains(scene_id.as_str()) {
                    return Err(VideoError::new(
                        VideoErrorCode::MissingReference,
                        "a music cue is anchored to a scene that does not exist",
                    )
                    .at("music_cues.anchor.scene_id"));
                }
            }
            CueAnchor::Turn { turn_id } => {
                if !turn_ids.contains(turn_id.as_str()) {
                    return Err(VideoError::new(
                        VideoErrorCode::MissingReference,
                        "a music cue is anchored to a dialogue turn that does not exist",
                    )
                    .at("music_cues.anchor.turn_id"));
                }
            }
            CueAnchor::AfterFinalTurn => {
                outros += 1;
                // Without a script there is no final line for an outro to resolve after.
                if !has_dialogue {
                    return Err(VideoError::new(
                        VideoErrorCode::MissingReference,
                        "an outro needs a script to play after",
                    )
                    .at("music_cues.anchor"));
                }
            }
        }
        if let Some(asset_id) = &cue.source_asset_id {
            if !music_asset_ids.contains(asset_id.as_str()) {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "a music cue references a source asset that is not registered music",
                )
                .at("music_cues.source_asset_id"));
            }
        }
    }
    // Two pieces of music both claiming to end the episode is an authoring mistake, not a layered
    // arrangement: the renderer would have to choose which one the episode actually ends on.
    if outros > 1 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCue,
            "an episode may end on only one outro",
        )
        .at("music_cues.anchor"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(id: &str, role: CueRole, anchor: CueAnchor, target_us: i64) -> MusicCue {
        MusicCue {
            id: id.into(),
            role,
            anchor,
            target_duration_us: Microseconds(target_us),
            direction: "warm, restrained, low strings".into(),
            source_asset_id: None,
            track_id: None,
            gain_db_milli: -6_000,
            fade_in_us: Microseconds(500_000),
            fade_out_us: Microseconds(1_000_000),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn scene_cue(id: &str, role: CueRole) -> MusicCue {
        cue(
            id,
            role,
            CueAnchor::Scene {
                scene_id: "scene-one".into(),
            },
            30_000_000,
        )
    }

    fn ids<'a>(values: &[&'a str]) -> BTreeSet<&'a str> {
        values.iter().copied().collect()
    }

    #[test]
    fn a_bed_always_ducks_against_the_speech_track() {
        let ducking = bed_ducking("speech");
        assert_eq!(ducking.sidechain_track_id, "speech");
        assert!(ducking.reduction_db_milli < 0);
        assert!(CueRole::Bed.is_underscore());
        assert!(!CueRole::Sting.is_underscore());
        assert!(!CueRole::Outro.is_underscore());
    }

    #[test]
    fn only_an_outro_may_end_the_episode() {
        cue(
            "outro",
            CueRole::Outro,
            CueAnchor::AfterFinalTurn,
            20_000_000,
        )
        .validate()
        .unwrap();

        let misplaced = cue(
            "sting",
            CueRole::Sting,
            CueAnchor::AfterFinalTurn,
            5_000_000,
        );
        assert_eq!(
            misplaced.validate().unwrap_err().code,
            VideoErrorCode::InvalidCue
        );

        let stranded = scene_cue("outro", CueRole::Outro);
        assert_eq!(
            stranded.validate().unwrap_err().code,
            VideoErrorCode::InvalidCue
        );
    }

    #[test]
    fn fades_cannot_together_exceed_the_cue() {
        let mut greedy = scene_cue("bed", CueRole::Bed);
        greedy.target_duration_us = Microseconds(1_000_000);
        greedy.fade_in_us = Microseconds(800_000);
        greedy.fade_out_us = Microseconds(800_000);
        assert_eq!(
            greedy.validate().unwrap_err().code,
            VideoErrorCode::InvalidCue
        );
    }

    #[test]
    fn music_that_runs_long_is_trimmed_with_a_tail_rather_than_cut() {
        let cue = scene_cue("bed", CueRole::Bed);
        let fit = fit_cue(&cue, Microseconds(45_000_000)).unwrap();
        assert_eq!(fit.action, CueFitAction::TrimmedWithTail);
        assert_eq!(fit.source_end_us, cue.target_duration_us);
        assert!(
            fit.fade_out_us.0 >= cue.fade_out_us.0,
            "a trim must carry at least the cue's own tail"
        );
        assert!(
            fit.fade_out_us.0 <= cue.target_duration_us.0 / 2,
            "a tail must not swallow the cue"
        );
    }

    #[test]
    fn music_that_runs_slightly_short_is_held_by_its_fade() {
        let cue = scene_cue("bed", CueRole::Bed);
        let fit = fit_cue(&cue, Microseconds(29_000_000)).unwrap();
        assert_eq!(fit.action, CueFitAction::HeldByFade);
        assert_eq!(fit.source_end_us, Microseconds(29_000_000));
    }

    #[test]
    fn music_that_falls_well_short_is_regenerated_rather_than_padded() {
        let cue = scene_cue("bed", CueRole::Bed);
        let error = fit_cue(&cue, Microseconds(20_000_000)).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::CueFitFailed);
    }

    #[test]
    fn music_with_no_measured_duration_cannot_be_fitted() {
        let cue = scene_cue("bed", CueRole::Bed);
        let error = fit_cue(&cue, Microseconds::ZERO).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCue);
    }

    #[test]
    fn a_cue_must_anchor_to_something_that_exists() {
        let scenes = ids(&["scene-one"]);
        let turns = ids(&["turn-a"]);
        let assets = BTreeSet::new();

        validate_cue_sheet(
            &[scene_cue("bed", CueRole::Bed)],
            &scenes,
            &turns,
            &assets,
            true,
        )
        .unwrap();

        let stray = cue(
            "bed",
            CueRole::Bed,
            CueAnchor::Scene {
                scene_id: "scene-absent".into(),
            },
            30_000_000,
        );
        let error = validate_cue_sheet(&[stray], &scenes, &turns, &assets, true).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);

        let stray_turn = cue(
            "sting",
            CueRole::Sting,
            CueAnchor::Turn {
                turn_id: "turn-absent".into(),
            },
            4_000_000,
        );
        let error = validate_cue_sheet(&[stray_turn], &scenes, &turns, &assets, true).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn an_outro_needs_a_script_to_play_after() {
        let outro = cue(
            "outro",
            CueRole::Outro,
            CueAnchor::AfterFinalTurn,
            20_000_000,
        );
        let error = validate_cue_sheet(
            std::slice::from_ref(&outro),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            false,
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);

        validate_cue_sheet(
            &[outro],
            &BTreeSet::new(),
            &ids(&["turn-a"]),
            &BTreeSet::new(),
            true,
        )
        .unwrap();
    }

    #[test]
    fn an_episode_may_end_on_only_one_outro() {
        let first = cue(
            "outro-a",
            CueRole::Outro,
            CueAnchor::AfterFinalTurn,
            20_000_000,
        );
        let second = cue(
            "outro-b",
            CueRole::Outro,
            CueAnchor::AfterFinalTurn,
            18_000_000,
        );
        let error = validate_cue_sheet(
            &[first, second],
            &BTreeSet::new(),
            &ids(&["turn-a"]),
            &BTreeSet::new(),
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCue);
    }

    #[test]
    fn a_cue_can_only_reference_registered_music() {
        let mut generated = scene_cue("bed", CueRole::Bed);
        generated.source_asset_id = Some("music-one".into());
        assert!(!generated.needs_generation());

        let error = validate_cue_sheet(
            std::slice::from_ref(&generated),
            &ids(&["scene-one"]),
            &BTreeSet::new(),
            &BTreeSet::new(),
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);

        validate_cue_sheet(
            &[generated],
            &ids(&["scene-one"]),
            &BTreeSet::new(),
            &ids(&["music-one"]),
            true,
        )
        .unwrap();
    }

    #[test]
    fn a_planned_cue_reports_that_it_still_needs_generation() {
        assert!(scene_cue("bed", CueRole::Bed).needs_generation());
    }

    #[test]
    fn a_cue_cannot_occupy_a_track_before_its_music_exists() {
        let mut premature = scene_cue("bed", CueRole::Bed);
        premature.track_id = Some("music-bed".into());
        assert_eq!(
            premature.validate().unwrap_err().code,
            VideoErrorCode::InvalidCue
        );

        premature.source_asset_id = Some("music-one".into());
        premature.validate().unwrap();
    }
}
