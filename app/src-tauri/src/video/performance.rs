//! Conversational timing between dialogue turns.
//!
//! Multi-voice dialogue stops sounding like concatenated clips when the silence between turns
//! carries meaning. A fast beat reads as a reply; a long one reads as hesitation or a reveal; a
//! small overlap reads as an interruption. This module derives those beats deterministically from
//! what the script already says - who speaks next, how the previous line ends, and what the stage
//! direction asks for - and lets the writer override any single beat.
//!
//! An explicit beat is keyed by turn id. Because a turn keeps its identifier when its words are
//! unchanged, an override survives every later edit to the rest of the script, which is exactly
//! the promise "the pause before her answer stays long" has to keep.

use super::cast::DialogueTurn;
use super::contracts::{
    validate_identifier, Microseconds, Validate, VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Ten seconds of held silence is already an extreme dramatic choice; beyond it a beat is far more
/// likely to be a mistake than an intention.
pub const MAX_BEAT_US: i64 = 10_000_000;
/// An overlap longer than two seconds stops reading as an interjection and starts destroying the
/// line underneath it.
pub const MAX_OVERLAP_US: i64 = 2_000_000;
/// The default interjection overlap when a direction asks for one without naming a length.
pub const DEFAULT_INTERJECTION_OVERLAP_US: i64 = 250_000;

/// The four beats a scripted conversation actually needs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceClock {
    /// Between two characters trading lines. The fastest beat: a reply.
    pub intra_exchange_us: Microseconds,
    /// When the same character continues. A new thought, not an answer.
    pub turn_of_thought_us: Microseconds,
    /// Before a line the script marks as landing: a reveal, a hesitation, a trailing-off answer.
    pub pre_reveal_us: Microseconds,
    /// Across a scene boundary.
    pub scene_boundary_us: Microseconds,
    /// The longest a reaction - a laugh, applause - may run before it is faded out. A voice
    /// model asked for a big laugh will happily laugh for fifteen seconds; a room does not.
    #[serde(default = "default_reaction_max_us")]
    pub reaction_max_us: Microseconds,
}

/// Three and a half seconds: long enough for a real laugh to land and settle, short enough that
/// the next line arrives while the room is still warm.
pub const DEFAULT_REACTION_MAX_US: i64 = 3_500_000;

fn default_reaction_max_us() -> Microseconds {
    Microseconds(DEFAULT_REACTION_MAX_US)
}

impl Default for PerformanceClock {
    fn default() -> Self {
        Self {
            intra_exchange_us: Microseconds(220_000),
            turn_of_thought_us: Microseconds(600_000),
            pre_reveal_us: Microseconds(1_200_000),
            scene_boundary_us: Microseconds(900_000),
            reaction_max_us: Microseconds(DEFAULT_REACTION_MAX_US),
        }
    }
}

impl Validate for PerformanceClock {
    fn validate(&self) -> VideoResult<()> {
        if !(500_000..=30_000_000).contains(&self.reaction_max_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "a reaction may run between half a second and thirty seconds",
            )
            .at("performance_clock.reaction_max_us"));
        }
        for (value, field) in [
            (
                self.intra_exchange_us,
                "performance_clock.intra_exchange_us",
            ),
            (
                self.turn_of_thought_us,
                "performance_clock.turn_of_thought_us",
            ),
            (self.pre_reveal_us, "performance_clock.pre_reveal_us"),
            (
                self.scene_boundary_us,
                "performance_clock.scene_boundary_us",
            ),
        ] {
            if !(0..=MAX_BEAT_US).contains(&value.0) {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidPerformance,
                    "a beat must be between zero and ten seconds",
                )
                .at(field));
            }
        }
        Ok(())
    }
}

/// Whether a beat was inferred from the script or chosen by the writer. A derived beat is
/// recomputed whenever the script changes; an explicit one is never silently overwritten.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeatSource {
    Derived,
    Explicit,
}

/// The silence - or overlap - immediately before one turn.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnBeat {
    pub turn_id: String,
    /// Silence held before this turn begins.
    pub lead_in_us: Microseconds,
    /// How far this turn starts before the previous one ends. An interjection.
    pub overlap_us: Microseconds,
    pub source: BeatSource,
}

impl Validate for TurnBeat {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.turn_id, "turn_beats.turn_id")?;
        if !(0..=MAX_BEAT_US).contains(&self.lead_in_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "a lead-in must be between zero and ten seconds",
            )
            .at("turn_beats.lead_in_us"));
        }
        if !(0..=MAX_OVERLAP_US).contains(&self.overlap_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "an overlap must be between zero and two seconds",
            )
            .at("turn_beats.overlap_us"));
        }
        // A turn cannot both wait and interrupt. Allowing both would leave the renderer to invent
        // a resolution, and two renderers would invent different ones.
        if self.lead_in_us.0 > 0 && self.overlap_us.0 > 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "a turn may hold a lead-in or overlap the previous turn, not both",
            )
            .at("turn_beats.overlap_us"));
        }
        Ok(())
    }
}

/// Derive a beat for every turn, preserving any explicit override.
///
/// The first turn always begins at zero: there is nothing for it to wait for.
pub fn derive_turn_beats(
    dialogue: &[DialogueTurn],
    clock: &PerformanceClock,
    explicit: &[TurnBeat],
) -> VideoResult<Vec<TurnBeat>> {
    clock.validate()?;
    let overrides = explicit
        .iter()
        .filter(|beat| matches!(beat.source, BeatSource::Explicit))
        .map(|beat| (beat.turn_id.as_str(), beat))
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut beats = Vec::with_capacity(dialogue.len());
    let mut previous: Option<&DialogueTurn> = None;
    for turn in dialogue {
        if let Some(explicit) = overrides.get(turn.id.as_str()) {
            explicit.validate()?;
            beats.push((*explicit).clone());
            previous = Some(turn);
            continue;
        }
        let beat = match previous {
            None => TurnBeat {
                turn_id: turn.id.clone(),
                lead_in_us: Microseconds::ZERO,
                overlap_us: Microseconds::ZERO,
                source: BeatSource::Derived,
            },
            Some(previous) => derive_beat(previous, turn, clock),
        };
        beat.validate()?;
        beats.push(beat);
        previous = Some(turn);
    }
    Ok(beats)
}

fn derive_beat(previous: &DialogueTurn, turn: &DialogueTurn, clock: &PerformanceClock) -> TurnBeat {
    let direction = turn.direction.as_deref().unwrap_or_default();
    // An interjection is the only direction that changes the shape of the beat rather than its
    // length, so it is tested before the pause rules.
    if implies_interjection(direction) {
        return TurnBeat {
            turn_id: turn.id.clone(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds(DEFAULT_INTERJECTION_OVERLAP_US),
            source: BeatSource::Derived,
        };
    }
    let lead_in = if turn.scene_id != previous.scene_id {
        clock.scene_boundary_us
    } else if implies_pause(direction) || trails_off(&previous.text) {
        clock.pre_reveal_us
    } else if turn.character_id == previous.character_id {
        clock.turn_of_thought_us
    } else {
        clock.intra_exchange_us
    };
    TurnBeat {
        turn_id: turn.id.clone(),
        lead_in_us: lead_in,
        overlap_us: Microseconds::ZERO,
        source: BeatSource::Derived,
    }
}

/// Directions writers actually use when they want the line held.
fn implies_pause(direction: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "pause",
        "beat",
        "silence",
        "hesitant",
        "hesitates",
        "after a moment",
        "slowly",
        "reluctant",
    ];
    let lowered = direction.to_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Directions that mean the line lands on top of the one before it.
fn implies_interjection(direction: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "interrupting",
        "interrupts",
        "cutting in",
        "cuts in",
        "overlapping",
    ];
    let lowered = direction.to_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// A line that trails off invites a longer silence than one that lands.
fn trails_off(text: &str) -> bool {
    let trimmed = text.trim_end();
    trimmed.ends_with("...")
        || trimmed.ends_with('\u{2026}')
        || trimmed.ends_with('\u{2014}')
        || trimmed.ends_with('\u{2013}')
}

/// Reject beats that do not describe the current script.
///
/// This runs inside `validate_strict`, which is on the hot path for every manifest load, revision,
/// and render admission. It is therefore a single pass over the beats against one prebuilt index:
/// a per-beat scan of the narration bindings would make validation quadratic in script length, and
/// a long episode is exactly where that cost would land.
///
/// The duration bound is deliberately checked against rendered takes rather than the script: an
/// overlap is only meaningful once there is audio for it to sit inside, and until then there is no
/// honest number to compare against.
pub(crate) fn validate_turn_beats(
    beats: &[TurnBeat],
    turn_positions: &BTreeMap<&str, usize>,
    ordered_turn_ids: &[&str],
    take_duration_us: &BTreeMap<&str, i64>,
) -> VideoResult<()> {
    let mut seen = BTreeSet::new();
    for beat in beats {
        beat.validate()?;
        let Some(index) = turn_positions.get(beat.turn_id.as_str()).copied() else {
            return Err(VideoError::new(
                VideoErrorCode::MissingReference,
                "a performance beat references a turn that is not in the script",
            )
            .at("turn_beats.turn_id"));
        };
        if !seen.insert(beat.turn_id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "a turn may have only one beat",
            )
            .at("turn_beats.turn_id"));
        }
        if beat.overlap_us.0 == 0 {
            continue;
        }
        // The first turn has nothing to overlap, and overlapping the whole of either take would
        // reorder the conversation rather than tighten it.
        if index == 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "the first turn has no previous turn to overlap",
            )
            .at("turn_beats.overlap_us"));
        }
        let previous_id = ordered_turn_ids[index - 1];
        let shorter = match (
            take_duration_us.get(beat.turn_id.as_str()).copied(),
            take_duration_us.get(previous_id).copied(),
        ) {
            (Some(current), Some(previous)) => current.min(previous),
            // Without both takes there is nothing measured to compare against, and inventing a
            // bound would be a fabricated constraint rather than a real one.
            _ => continue,
        };
        if beat.overlap_us.0 >= shorter {
            return Err(VideoError::new(
                VideoErrorCode::InvalidPerformance,
                "an overlap must be shorter than both the turns it joins",
            )
            .at("turn_beats.overlap_us"));
        }
    }
    Ok(())
}

/// Index a script once for beat validation. Callers that already walk the dialogue should reuse
/// their own pass rather than calling this a second time.
pub(crate) fn index_turns(dialogue: &[DialogueTurn]) -> (BTreeMap<&str, usize>, Vec<&str>) {
    let ordered = dialogue
        .iter()
        .map(|turn| turn.id.as_str())
        .collect::<Vec<_>>();
    let positions = ordered
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect::<BTreeMap<_, _>>();
    (positions, ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(
        id: &str,
        order: u32,
        character: &str,
        text: &str,
        direction: Option<&str>,
    ) -> DialogueTurn {
        DialogueTurn {
            id: id.into(),
            scene_id: None,
            order,
            character_id: character.into(),
            text: text.into(),
            direction: direction.map(str::to_string),
            source_line: order + 1,
            revision: 1,
        }
    }

    /// Validate beats against a script the way the manifest does, without rendered takes.
    fn check(beats: &[TurnBeat], dialogue: &[DialogueTurn]) -> VideoResult<()> {
        let (positions, ordered) = index_turns(dialogue);
        validate_turn_beats(beats, &positions, &ordered, &BTreeMap::new())
    }

    /// Validate beats against a script whose takes have known measured durations.
    fn check_with_takes(
        beats: &[TurnBeat],
        dialogue: &[DialogueTurn],
        durations: &[(&str, i64)],
    ) -> VideoResult<()> {
        let (positions, ordered) = index_turns(dialogue);
        let takes = durations.iter().copied().collect::<BTreeMap<_, _>>();
        validate_turn_beats(beats, &positions, &ordered, &takes)
    }

    #[test]
    fn the_first_turn_waits_for_nothing() {
        let dialogue = vec![turn("t1", 0, "narrator", "It began.", None)];
        let beats = derive_turn_beats(&dialogue, &PerformanceClock::default(), &[]).unwrap();
        assert_eq!(beats[0].lead_in_us, Microseconds::ZERO);
        assert_eq!(beats[0].source, BeatSource::Derived);
    }

    #[test]
    fn a_reply_is_faster_than_the_same_speaker_continuing() {
        let clock = PerformanceClock::default();
        let reply = vec![
            turn("t1", 0, "narrator", "She waited.", None),
            turn("t2", 1, "adaeze", "I am here.", None),
        ];
        let continued = vec![
            turn("t1", 0, "narrator", "She waited.", None),
            turn("t2", 1, "narrator", "And waited.", None),
        ];
        let reply_beats = derive_turn_beats(&reply, &clock, &[]).unwrap();
        let continued_beats = derive_turn_beats(&continued, &clock, &[]).unwrap();
        assert_eq!(reply_beats[1].lead_in_us, clock.intra_exchange_us);
        assert_eq!(continued_beats[1].lead_in_us, clock.turn_of_thought_us);
        assert!(reply_beats[1].lead_in_us.0 < continued_beats[1].lead_in_us.0);
    }

    #[test]
    fn a_line_that_trails_off_earns_a_longer_beat() {
        let clock = PerformanceClock::default();
        let dialogue = vec![
            turn("t1", 0, "adaeze", "I thought you would...", None),
            turn("t2", 1, "narrator", "She did not finish.", None),
        ];
        let beats = derive_turn_beats(&dialogue, &clock, &[]).unwrap();
        assert_eq!(beats[1].lead_in_us, clock.pre_reveal_us);
    }

    #[test]
    fn a_pause_direction_holds_the_line() {
        let clock = PerformanceClock::default();
        let dialogue = vec![
            turn("t1", 0, "narrator", "He asked her name.", None),
            turn("t2", 1, "adaeze", "Adaeze.", Some("after a moment")),
        ];
        let beats = derive_turn_beats(&dialogue, &clock, &[]).unwrap();
        assert_eq!(beats[1].lead_in_us, clock.pre_reveal_us);
    }

    #[test]
    fn an_interjection_overlaps_instead_of_waiting() {
        let dialogue = vec![
            turn("t1", 0, "narrator", "She began to explain that", None),
            turn("t2", 1, "adaeze", "No.", Some("interrupting")),
        ];
        let beats = derive_turn_beats(&dialogue, &PerformanceClock::default(), &[]).unwrap();
        assert_eq!(beats[1].lead_in_us, Microseconds::ZERO);
        assert_eq!(
            beats[1].overlap_us,
            Microseconds(DEFAULT_INTERJECTION_OVERLAP_US)
        );
    }

    #[test]
    fn an_explicit_beat_survives_derivation() {
        let dialogue = vec![
            turn("t1", 0, "narrator", "He asked her name.", None),
            turn("t2", 1, "adaeze", "Adaeze.", None),
        ];
        let explicit = vec![TurnBeat {
            turn_id: "t2".into(),
            lead_in_us: Microseconds(3_000_000),
            overlap_us: Microseconds::ZERO,
            source: BeatSource::Explicit,
        }];
        let beats = derive_turn_beats(&dialogue, &PerformanceClock::default(), &explicit).unwrap();
        assert_eq!(beats[1].lead_in_us, Microseconds(3_000_000));
        assert_eq!(beats[1].source, BeatSource::Explicit);
    }

    #[test]
    fn a_derived_beat_is_never_treated_as_an_override() {
        let dialogue = vec![
            turn("t1", 0, "narrator", "He asked her name.", None),
            turn("t2", 1, "adaeze", "Adaeze.", None),
        ];
        // A stale derived beat from an earlier script must be recomputed, not preserved.
        let stale = vec![TurnBeat {
            turn_id: "t2".into(),
            lead_in_us: Microseconds(9_000_000),
            overlap_us: Microseconds::ZERO,
            source: BeatSource::Derived,
        }];
        let beats = derive_turn_beats(&dialogue, &PerformanceClock::default(), &stale).unwrap();
        assert_eq!(
            beats[1].lead_in_us,
            PerformanceClock::default().intra_exchange_us
        );
    }

    #[test]
    fn a_turn_cannot_both_wait_and_interrupt() {
        let beat = TurnBeat {
            turn_id: "t2".into(),
            lead_in_us: Microseconds(200_000),
            overlap_us: Microseconds(200_000),
            source: BeatSource::Explicit,
        };
        assert_eq!(
            beat.validate().unwrap_err().code,
            VideoErrorCode::InvalidPerformance
        );
    }

    #[test]
    fn the_first_turn_cannot_overlap() {
        let dialogue = vec![
            turn("t1", 0, "narrator", "It began.", None),
            turn("t2", 1, "adaeze", "I am here.", None),
        ];
        let beats = vec![TurnBeat {
            turn_id: "t1".into(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds(100_000),
            source: BeatSource::Explicit,
        }];
        let error = check(&beats, &dialogue).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);
    }

    #[test]
    fn an_overlap_cannot_swallow_the_shorter_take() {
        let dialogue = vec![
            turn("t1", 0, "narrator", "It began.", None),
            turn("t2", 1, "adaeze", "No.", Some("interrupting")),
        ];
        let beats = vec![TurnBeat {
            turn_id: "t2".into(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds(400_000),
            source: BeatSource::Explicit,
        }];
        // The interjection take is only 300 ms long, so a 400 ms overlap would reorder the two.
        let durations = [("t1", 2_000_000i64), ("t2", 300_000)];
        let error = check_with_takes(&beats, &dialogue, &durations).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);

        let safe = vec![TurnBeat {
            turn_id: "t2".into(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds(200_000),
            source: BeatSource::Explicit,
        }];
        check_with_takes(&safe, &dialogue, &durations).unwrap();
    }

    #[test]
    fn a_beat_cannot_reference_a_turn_outside_the_script() {
        let dialogue = vec![turn("t1", 0, "narrator", "It began.", None)];
        let beats = vec![TurnBeat {
            turn_id: "t-absent".into(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds::ZERO,
            source: BeatSource::Explicit,
        }];
        let error = check(&beats, &dialogue).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn a_turn_may_have_only_one_beat() {
        let dialogue = vec![turn("t1", 0, "narrator", "It began.", None)];
        let beat = TurnBeat {
            turn_id: "t1".into(),
            lead_in_us: Microseconds::ZERO,
            overlap_us: Microseconds::ZERO,
            source: BeatSource::Explicit,
        };
        let error = check(&[beat.clone(), beat], &dialogue).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);
    }
}
