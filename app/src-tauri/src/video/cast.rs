//! Canonical cast and speaker-attributed dialogue contracts.
//!
//! A `CastMember` binds one named character to the exact voice, model, language, and delivery
//! defaults that perform it, so the same character sounds the same in every take of every
//! episode. A `DialogueTurn` is one character's uninterrupted contribution to the script and is
//! the smallest unit of narration soundAr renders, caches, and invalidates.
//!
//! Turn-scoped narration is what makes a multi-character production revisable: re-reading one
//! line invalidates that line's take, not the scene that contains it. Parsing is deliberately
//! strict. An unknown speaker, an empty turn, or an unbalanced parenthetical is reported against
//! its source line rather than silently narrated by whichever voice happened to be selected.

use super::contracts::{
    validate_identifier, validate_language_tag, validate_nonempty, validate_timestamp_text,
    Validate, VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_CAST_MEMBERS: usize = 32;
pub const MAX_DIALOGUE_TURNS: usize = 5_000;
pub const MAX_TURN_TEXT_BYTES: usize = 8_000;
pub const MAX_DIRECTION_BYTES: usize = 500;
pub const MAX_SCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SPEAKER_NAME_BYTES: usize = 64;

/// Delivery is stored in milli-units so a saved performance is reproducible without
/// float drift across a save, reload, and re-render.
pub const NATURAL_RATE_MILLI: i32 = 1_000;
pub const MIN_RATE_MILLI: i32 = 250;
pub const MAX_RATE_MILLI: i32 = 4_000;
pub const MAX_ABS_PITCH_MILLI: i32 = 1_000;
pub const MAX_ENERGY_MILLI: i32 = 2_000;

/// Per-character delivery defaults.
///
/// These are defaults, not guarantees. An engine that does not declare support for a control
/// ignores it; the recorded value still travels with the take so a later re-render on a capable
/// engine reproduces the intended performance rather than guessing at it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CastDelivery {
    pub rate_milli: i32,
    pub pitch_milli: i32,
    pub energy_milli: i32,
}

impl Default for CastDelivery {
    fn default() -> Self {
        Self {
            rate_milli: NATURAL_RATE_MILLI,
            pitch_milli: 0,
            energy_milli: NATURAL_RATE_MILLI,
        }
    }
}

impl Validate for CastDelivery {
    fn validate(&self) -> VideoResult<()> {
        if !(MIN_RATE_MILLI..=MAX_RATE_MILLI).contains(&self.rate_milli) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCast,
                "delivery rate must be between 0.25x and 4x natural speed",
            )
            .at("cast.delivery.rate_milli"));
        }
        if self.pitch_milli.abs() > MAX_ABS_PITCH_MILLI {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCast,
                "delivery pitch offset is outside the supported range",
            )
            .at("cast.delivery.pitch_milli"));
        }
        if !(0..=MAX_ENERGY_MILLI).contains(&self.energy_milli) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCast,
                "delivery energy is outside the supported range",
            )
            .at("cast.delivery.energy_milli"));
        }
        Ok(())
    }
}

/// One named character bound to the exact route that performs it.
///
/// `name` is the token used in the script (`ADAEZE: ...`). It is matched case-insensitively so a
/// writer is not forced to keep capitalization consistent, but it is stored exactly as declared
/// so the cast list reads the way the author wrote it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CastMember {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub voice_id: String,
    pub model_id: String,
    pub language: String,
    #[serde(default)]
    pub delivery: CastDelivery,
    /// Present when this character is performed by a consent-backed managed voice reference.
    /// A cloned voice without consent evidence is rejected before it can reach a render.
    #[serde(default)]
    pub consent_reference_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: String,
}

impl CastMember {
    /// The comparison key used to resolve a script's speaker token to this character.
    pub fn match_key(&self) -> String {
        normalize_speaker_name(&self.name)
    }
}

impl Validate for CastMember {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "cast.id")?;
        validate_speaker_name(&self.name, "cast.name")?;
        validate_nonempty(&self.display_name, "cast.display_name", 256)?;
        validate_identifier(&self.voice_id, "cast.voice_id")?;
        validate_nonempty(&self.model_id, "cast.model_id", 256)?;
        validate_language_tag(&self.language, "cast.language")?;
        self.delivery.validate()?;
        if let Some(reference_id) = &self.consent_reference_id {
            validate_identifier(reference_id, "cast.consent_reference_id")?;
        }
        if let Some(notes) = &self.notes {
            if notes.len() > 2_000 {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidCast,
                    "cast notes are limited to 2000 bytes",
                )
                .at("cast.notes"));
            }
        }
        validate_timestamp_text(&self.created_at, "cast.created_at")?;
        Ok(())
    }
}

/// One character's uninterrupted contribution to the script.
///
/// `text` is exactly what the voice is asked to speak. A parenthetical `direction` steers
/// performance and beat selection but is never spoken, so it is stored separately rather than
/// stripped and forgotten.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DialogueTurn {
    pub id: String,
    #[serde(default)]
    pub scene_id: Option<String>,
    pub order: u32,
    pub character_id: String,
    pub text: String,
    #[serde(default)]
    pub direction: Option<String>,
    /// 1-indexed line in the script this turn was parsed from, so a rejection or a later
    /// revision can point the author at the exact line they wrote.
    pub source_line: u32,
    #[serde(default)]
    pub revision: u32,
}

impl DialogueTurn {
    /// The exact bytes a narration take for this turn must have been asked to speak.
    pub fn spoken_text(&self) -> &str {
        self.text.as_str()
    }
}

impl Validate for DialogueTurn {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "dialogue.id")?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "dialogue.scene_id")?;
        }
        validate_identifier(&self.character_id, "dialogue.character_id")?;
        if self.text.trim().is_empty() || self.text.len() > MAX_TURN_TEXT_BYTES {
            return Err(VideoError::new(
                VideoErrorCode::InvalidDialogue,
                format!("turn text must be non-empty and at most {MAX_TURN_TEXT_BYTES} bytes"),
            )
            .at("dialogue.text"));
        }
        if let Some(direction) = &self.direction {
            if direction.trim().is_empty() || direction.len() > MAX_DIRECTION_BYTES {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidDialogue,
                    format!(
                        "a stage direction must be non-empty and at most {MAX_DIRECTION_BYTES} bytes"
                    ),
                )
                .at("dialogue.direction"));
            }
        }
        if self.source_line == 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidDialogue,
                "source_line is 1-indexed and cannot be zero",
            )
            .at("dialogue.source_line"));
        }
        Ok(())
    }
}

/// Resolve every declared cast member by its script token.
///
/// Two characters that differ only by capitalization or surrounding whitespace would make the
/// script ambiguous, so the collision is rejected here rather than resolved arbitrarily at parse
/// time.
pub fn index_cast_by_name(cast: &[CastMember]) -> VideoResult<BTreeMap<String, &CastMember>> {
    let mut index = BTreeMap::new();
    for member in cast {
        member.validate()?;
        if index.insert(member.match_key(), member).is_some() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCast,
                format!(
                    "two cast members share the script name {}; names must be distinguishable",
                    member.name
                ),
            )
            .at("cast.name"));
        }
    }
    Ok(index)
}

/// A parsed turn before it is assigned a durable identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTurn {
    pub character_id: String,
    pub speaker_name: String,
    pub text: String,
    pub direction: Option<String>,
    pub source_line: u32,
}

/// Parse a speaker-attributed script into ordered turns.
///
/// Accepted shape:
///
/// ```text
/// NARRATOR: The harmattan came early that year.
/// ADAEZE: (quiet) You said you would come back before the rains.
///   And you did not.
/// ```
///
/// A line beginning `NAME:` opens a turn. Following non-empty lines continue it. A blank line
/// closes it. A parenthetical at the start of a turn becomes its direction. Every rejection names
/// the offending 1-indexed source line.
pub fn parse_dialogue_script(script: &str, cast: &[CastMember]) -> VideoResult<Vec<ParsedTurn>> {
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!("a script is limited to {MAX_SCRIPT_BYTES} bytes"),
        )
        .at("script"));
    }
    let index = index_cast_by_name(cast)?;
    if index.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCast,
            "a dialogue script requires at least one cast member",
        )
        .at("cast"));
    }

    let mut turns: Vec<ParsedTurn> = Vec::new();
    let mut open: Option<ParsedTurn> = None;

    for (offset, raw_line) in script.lines().enumerate() {
        let line_number = u32::try_from(offset + 1).map_err(|_| {
            VideoError::new(
                VideoErrorCode::InvalidDialogue,
                "the script has more lines than soundAr can address",
            )
            .at("script")
        })?;
        // `lines()` already removes `\n`; a CRLF file leaves the `\r` behind.
        let line = raw_line.trim_end_matches('\r').trim_end();

        if line.trim().is_empty() {
            if let Some(turn) = open.take() {
                turns.push(finish_turn(turn)?);
            }
            continue;
        }

        // A colon alone cannot mean "new speaker" - prose says "and then she said: run". A header
        // is a token that names someone in the cast, or a screenplay-style all-capitals cue whose
        // name we then require to be castable so a typo is reported instead of silently spoken.
        let header = match split_speaker_header(line) {
            Some((speaker, remainder)) if index.contains_key(&normalize_speaker_name(speaker)) => {
                Some((speaker, remainder))
            }
            Some((speaker, remainder)) if is_screenplay_cue(speaker) => Some((speaker, remainder)),
            _ => None,
        };

        match header {
            Some((speaker, remainder)) => {
                let member = index.get(&normalize_speaker_name(speaker)).ok_or_else(|| {
                    VideoError::new(
                        VideoErrorCode::UnknownSpeaker,
                        format!(
                            "line {line_number} is spoken by {speaker}, who is not in the cast"
                        ),
                    )
                    .at("script")
                })?;
                if let Some(turn) = open.take() {
                    turns.push(finish_turn(turn)?);
                }
                if turns.len() >= MAX_DIALOGUE_TURNS {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidDialogue,
                        format!("a script is limited to {MAX_DIALOGUE_TURNS} turns"),
                    )
                    .at("script"));
                }
                open = Some(ParsedTurn {
                    character_id: member.id.clone(),
                    speaker_name: member.name.clone(),
                    text: remainder.trim().to_string(),
                    direction: None,
                    source_line: line_number,
                });
            }
            None => {
                let Some(turn) = open.as_mut() else {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidDialogue,
                        format!(
                            "line {line_number} has no speaker; every line must follow a NAME: header"
                        ),
                    )
                    .at("script"));
                };
                if !turn.text.is_empty() {
                    turn.text.push(' ');
                }
                turn.text.push_str(line.trim());
                if turn.text.len() > MAX_TURN_TEXT_BYTES {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidDialogue,
                        format!(
                            "the turn continuing at line {line_number} exceeds {MAX_TURN_TEXT_BYTES} bytes"
                        ),
                    )
                    .at("script"));
                }
            }
        }
    }

    if let Some(turn) = open.take() {
        turns.push(finish_turn(turn)?);
    }

    if turns.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            "the script contains no dialogue turns",
        )
        .at("script"));
    }
    if turns.len() > MAX_DIALOGUE_TURNS {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!("a script is limited to {MAX_DIALOGUE_TURNS} turns"),
        )
        .at("script"));
    }
    Ok(turns)
}

/// Extract a leading parenthetical direction and reject a turn that says nothing.
fn finish_turn(mut turn: ParsedTurn) -> VideoResult<ParsedTurn> {
    let (direction, spoken) = split_leading_direction(&turn.text, turn.source_line)?;
    turn.direction = direction;
    turn.text = spoken;
    if turn.text.trim().is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!(
                "the turn opened at line {} has a speaker but nothing to say",
                turn.source_line
            ),
        )
        .at("script"));
    }
    if turn.text.len() > MAX_TURN_TEXT_BYTES {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!(
                "the turn opened at line {} exceeds {MAX_TURN_TEXT_BYTES} bytes",
                turn.source_line
            ),
        )
        .at("script"));
    }
    Ok(turn)
}

/// Split `NAME:` from the rest of a line.
///
/// The speaker token is deliberately narrow. Prose containing a colon - "She said: run" - must not
/// be mistaken for a speaker header, so a token containing a sentence-like character is rejected
/// here and the line is treated as continuation text.
fn split_speaker_header(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let colon = trimmed.find(':')?;
    let (speaker, remainder) = trimmed.split_at(colon);
    let speaker = speaker.trim();
    if speaker.is_empty() || speaker.len() > MAX_SPEAKER_NAME_BYTES {
        return None;
    }
    if !speaker.chars().all(is_speaker_char) {
        return None;
    }
    Some((speaker, remainder.get(1..).unwrap_or_default()))
}

/// A screenplay speaker cue is written in capitals. Requiring at least one letter and no
/// lowercase letter keeps ordinary prose containing a colon from being read as a new speaker,
/// while still catching a misspelled character name instead of narrating it with the wrong voice.
fn is_screenplay_cue(value: &str) -> bool {
    value.chars().any(char::is_alphabetic) && !value.chars().any(char::is_lowercase)
}

fn is_speaker_char(value: char) -> bool {
    value.is_alphanumeric() || matches!(value, ' ' | '_' | '-' | '.' | '\'')
}

/// Take a leading `(...)` direction off a turn, rejecting an unbalanced parenthetical rather than
/// speaking a stray bracket.
fn split_leading_direction(text: &str, line: u32) -> VideoResult<(Option<String>, String)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('(') {
        return Ok((None, text.trim().to_string()));
    }
    let Some(close) = trimmed.find(')') else {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!("the stage direction opened at line {line} is never closed"),
        )
        .at("script"));
    };
    let direction = trimmed[1..close].trim().to_string();
    if direction.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!("the stage direction at line {line} is empty"),
        )
        .at("script"));
    }
    if direction.len() > MAX_DIRECTION_BYTES {
        return Err(VideoError::new(
            VideoErrorCode::InvalidDialogue,
            format!("the stage direction at line {line} exceeds {MAX_DIRECTION_BYTES} bytes"),
        )
        .at("script"));
    }
    Ok((
        Some(direction),
        trimmed
            .get(close + 1..)
            .unwrap_or_default()
            .trim()
            .to_string(),
    ))
}

pub(crate) fn normalize_speaker_name(value: &str) -> String {
    value.trim().to_lowercase()
}

fn validate_speaker_name(value: &str, field: &str) -> VideoResult<()> {
    let trimmed = value.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= MAX_SPEAKER_NAME_BYTES
        && trimmed.chars().all(is_speaker_char);
    if !valid {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCast,
            "a cast name must be 1..=64 letters, digits, spaces, or '-_.\\''",
        )
        .at(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, name: &str) -> CastMember {
        CastMember {
            id: id.into(),
            name: name.into(),
            display_name: name.into(),
            voice_id: "af-heart".into(),
            model_id: "kokoro-82m".into(),
            language: "en-US".into(),
            delivery: CastDelivery::default(),
            consent_reference_id: None,
            notes: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn cast() -> Vec<CastMember> {
        vec![member("narrator", "NARRATOR"), member("adaeze", "ADAEZE")]
    }

    #[test]
    fn parses_speaker_turns_and_directions() {
        let script = "NARRATOR: The harmattan came early.\n\nADAEZE: (quiet) You said you would come back.\n  And you did not.\n";
        let turns = parse_dialogue_script(script, &cast()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].character_id, "narrator");
        assert_eq!(turns[0].text, "The harmattan came early.");
        assert_eq!(turns[0].direction, None);
        assert_eq!(turns[0].source_line, 1);
        assert_eq!(turns[1].character_id, "adaeze");
        assert_eq!(turns[1].direction.as_deref(), Some("quiet"));
        assert_eq!(
            turns[1].text,
            "You said you would come back. And you did not."
        );
        assert_eq!(turns[1].source_line, 3);
    }

    #[test]
    fn matches_speaker_names_case_insensitively() {
        let turns = parse_dialogue_script("Narrator: One line.\n", &cast()).unwrap();
        assert_eq!(turns[0].character_id, "narrator");
        assert_eq!(turns[0].speaker_name, "NARRATOR");
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let turns = parse_dialogue_script("NARRATOR: One.\r\nTwo.\r\n", &cast()).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "One. Two.");
    }

    #[test]
    fn rejects_an_unknown_speaker_by_line() {
        let error = parse_dialogue_script("EMEKA: Who am I?\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::UnknownSpeaker);
        assert!(error.message.contains("line 1"), "{}", error.message);
        assert!(error.message.contains("EMEKA"), "{}", error.message);
    }

    #[test]
    fn rejects_text_before_any_speaker() {
        let error = parse_dialogue_script("Once upon a time.\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
        assert!(error.message.contains("line 1"), "{}", error.message);
    }

    #[test]
    fn rejects_an_empty_turn_by_line() {
        let error = parse_dialogue_script("NARRATOR: One.\n\nADAEZE:\n\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
        assert!(error.message.contains("line 3"), "{}", error.message);
    }

    #[test]
    fn rejects_an_unbalanced_direction_by_line() {
        let error = parse_dialogue_script("ADAEZE: (quiet You said.\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
        assert!(error.message.contains("line 1"), "{}", error.message);
    }

    #[test]
    fn rejects_a_direction_only_turn() {
        let error = parse_dialogue_script("ADAEZE: (quiet)\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
        assert!(
            error.message.contains("nothing to say"),
            "{}",
            error.message
        );
    }

    #[test]
    fn prose_containing_a_colon_continues_the_current_turn() {
        let turns =
            parse_dialogue_script("NARRATOR: She spoke.\nAnd then she said: run.\n", &cast())
                .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "She spoke. And then she said: run.");
    }

    #[test]
    fn rejects_an_empty_script() {
        let error = parse_dialogue_script("\n\n", &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
    }

    #[test]
    fn rejects_a_cast_with_indistinguishable_names() {
        let ambiguous = vec![member("a", "NARRATOR"), member("b", "narrator")];
        let error = index_cast_by_name(&ambiguous).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCast);
    }

    #[test]
    fn rejects_a_script_with_no_cast() {
        let error = parse_dialogue_script("NARRATOR: One.\n", &[]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCast);
    }

    #[test]
    fn rejects_delivery_outside_the_supported_envelope() {
        let mut fast = CastDelivery::default();
        fast.rate_milli = 9_000;
        assert_eq!(
            fast.validate().unwrap_err().code,
            VideoErrorCode::InvalidCast
        );
        let mut loud = CastDelivery::default();
        loud.energy_milli = -1;
        assert_eq!(
            loud.validate().unwrap_err().code,
            VideoErrorCode::InvalidCast
        );
    }

    #[test]
    fn rejects_a_script_larger_than_the_supported_envelope() {
        let script = "NARRATOR: ".to_string() + &"a".repeat(MAX_SCRIPT_BYTES);
        let error = parse_dialogue_script(&script, &cast()).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidDialogue);
    }
}
