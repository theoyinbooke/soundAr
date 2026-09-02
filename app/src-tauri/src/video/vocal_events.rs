//! Vocal events: the laughs, sighs, and breaths a script asks a voice to perform.
//!
//! A writer marks these however they were taught to - `(laughs)`, `[chuckles]`, `*giggles*`,
//! `[Laughter]` - and a voice model, if it can perform them at all, understands exactly one
//! spelling of exactly one vocabulary. Breeze TTS 2 is trained on `(laugh)`; Chatterbox Turbo on
//! `[laugh]`; Kokoro on nothing, and reads whatever it is given as words. Left alone, the cue is
//! either dropped or, worse, narrated: an audience that says "Laughter" out loud.
//!
//! This module holds the one canonical form soundAr stores - `(laugh)` inline, lower case,
//! singular - and renders it into whatever an engine was trained on at the moment a take is
//! requested. A cue an engine cannot perform is removed rather than spoken, and the removal is
//! reported so the writer knows the voice they cast cannot laugh.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One thing a voice can do that is not a word.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VocalEvent {
    Laugh,
    Chuckle,
    Giggle,
    Sigh,
    Cough,
    ClearsThroat,
    Gasp,
    Breath,
    Hmm,
    Applause,
}

impl VocalEvent {
    pub const ALL: [VocalEvent; 10] = [
        VocalEvent::Laugh,
        VocalEvent::Chuckle,
        VocalEvent::Giggle,
        VocalEvent::Sigh,
        VocalEvent::Cough,
        VocalEvent::ClearsThroat,
        VocalEvent::Gasp,
        VocalEvent::Breath,
        VocalEvent::Hmm,
        VocalEvent::Applause,
    ];

    /// The canonical spelling soundAr stores, without any brackets.
    pub fn canonical(self) -> &'static str {
        match self {
            VocalEvent::Laugh => "laugh",
            VocalEvent::Chuckle => "chuckle",
            VocalEvent::Giggle => "giggle",
            VocalEvent::Sigh => "sigh",
            VocalEvent::Cough => "cough",
            VocalEvent::ClearsThroat => "clears throat",
            VocalEvent::Gasp => "gasp",
            VocalEvent::Breath => "breath",
            VocalEvent::Hmm => "hmm",
            VocalEvent::Applause => "applause",
        }
    }

    /// The canonical token as it appears inside a stored turn: `(laugh)`.
    pub fn token(self) -> String {
        format!("({})", self.canonical())
    }

    /// Recognise a cue however the writer spelled it. `None` means the text between the brackets
    /// was not a vocal event: a stage direction, a sound effect, or prose.
    pub fn from_cue(cue: &str) -> Option<Self> {
        let key = cue
            .trim()
            .to_lowercase()
            .chars()
            .filter(|character| character.is_alphanumeric() || character.is_whitespace())
            .collect::<String>();
        let key = key.split_whitespace().collect::<Vec<_>>().join(" ");
        Some(match key.as_str() {
            "laugh"
            | "laughs"
            | "laughing"
            | "laughter"
            | "laughed"
            | "lol"
            | "haha"
            | "hahaha"
            | "ha ha"
            | "ha ha ha"
            | "big laugh"
            | "bursts out laughing"
            | "laughs out loud" => VocalEvent::Laugh,
            "chuckle" | "chuckles" | "chuckling" | "soft laugh" | "small laugh" | "quiet laugh"
            | "snicker" | "snickers" => VocalEvent::Chuckle,
            "giggle" | "giggles" | "giggling" | "titter" | "titters" => VocalEvent::Giggle,
            "sigh" | "sighs" | "sighing" | "exhales" | "exhale" | "heavy sigh" => VocalEvent::Sigh,
            "cough" | "coughs" | "coughing" => VocalEvent::Cough,
            "clears throat" | "clear throat" | "clearing throat" | "ahem" => {
                VocalEvent::ClearsThroat
            }
            "gasp" | "gasps" | "gasping" | "sharp intake of breath" => VocalEvent::Gasp,
            "breath"
            | "breathes"
            | "breathing"
            | "inhale"
            | "inhales"
            | "deep breath"
            | "takes a breath"
            | "takes a deep breath" => VocalEvent::Breath,
            "hmm" | "hm" | "hmmm" | "mm" | "mmm" => VocalEvent::Hmm,
            "applause"
            | "applauds"
            | "clap"
            | "claps"
            | "clapping"
            | "cheers"
            | "cheering"
            | "cheers and applause"
            | "laughter and applause"
            | "applause and laughter" => VocalEvent::Applause,
            _ => return None,
        })
    }

    /// Words a recogniser produces when it hears this event performed rather than spoken. Used by
    /// quality control to tell a laugh from the word "laugh".
    pub fn heard_as(self) -> &'static [&'static str] {
        match self {
            VocalEvent::Laugh | VocalEvent::Chuckle | VocalEvent::Giggle => &[
                "ha", "haha", "hahaha", "hah", "heh", "hehe", "hehehe", "ah", "aha", "oh", "ho",
                "hoho", "hee", "hihi", "huh", "hm", "hmm",
            ],
            VocalEvent::Sigh | VocalEvent::Breath | VocalEvent::Gasp => &[
                "ah", "ahh", "oh", "ooh", "hah", "huh", "phew", "whew", "hm", "hmm", "mm",
            ],
            VocalEvent::Cough | VocalEvent::ClearsThroat => &["ahem", "hm", "hmm", "uh", "ugh"],
            VocalEvent::Hmm => &["hm", "hmm", "mm", "mmm", "um", "uh"],
            VocalEvent::Applause => &["yeah", "yay", "woo", "whoo", "wow", "oh"],
        }
    }

    /// The words a voice would say if it read the cue as text. Hearing one of these where the
    /// script has an event means the event was narrated, not performed.
    pub fn misread_as(self) -> &'static [&'static str] {
        match self {
            VocalEvent::Laugh => &["laugh", "laughs", "laughter", "laughing"],
            VocalEvent::Chuckle => &["chuckle", "chuckles", "chuckling"],
            VocalEvent::Giggle => &["giggle", "giggles", "giggling"],
            VocalEvent::Sigh => &["sigh", "sighs", "sighing"],
            VocalEvent::Cough => &["cough", "coughs", "coughing"],
            VocalEvent::ClearsThroat => &["clears", "throat", "clearing"],
            VocalEvent::Gasp => &["gasp", "gasps", "gasping"],
            VocalEvent::Breath => &["breath", "breathes", "breathing", "inhale", "inhales"],
            VocalEvent::Hmm => &[],
            VocalEvent::Applause => &["applause", "applauds", "clap", "claps", "clapping"],
        }
    }
}

impl fmt::Display for VocalEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical())
    }
}

/// The spelling an engine was trained on. Declared per engine in `data/engine_manifests.json`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VocalVocabulary {
    /// The engine performs no events; every cue is removed before synthesis.
    #[default]
    None,
    /// `(laugh)` - Breeze TTS 2.
    Parenthesis,
    /// `[laugh]` - Chatterbox Turbo.
    Bracket,
}

impl VocalVocabulary {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "none" | "" => Some(VocalVocabulary::None),
            "parenthesis" | "parentheses" | "paren" => Some(VocalVocabulary::Parenthesis),
            "bracket" | "brackets" | "square" => Some(VocalVocabulary::Bracket),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VocalVocabulary::None => "none",
            VocalVocabulary::Parenthesis => "parenthesis",
            VocalVocabulary::Bracket => "bracket",
        }
    }

    pub fn performs_events(self) -> bool {
        !matches!(self, VocalVocabulary::None)
    }

    /// How this vocabulary writes one event, or `None` when it cannot write it at all.
    pub fn render(self, event: VocalEvent) -> Option<String> {
        match self {
            VocalVocabulary::None => None,
            VocalVocabulary::Parenthesis => Some(format!("({})", event.canonical())),
            VocalVocabulary::Bracket => Some(format!("[{}]", event.canonical())),
        }
    }
}

/// A turn's text split into the words to speak and the events to perform, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptSegment {
    Words(String),
    Event(VocalEvent),
}

/// The result of reading a writer's line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CueParse {
    /// The line in soundAr's canonical form: words as written, events as `(laugh)` tokens.
    pub canonical: String,
    /// Bracketed notes that were not vocal events - `[SFX: door slams]`, `*beat*`. They steer
    /// performance and are never spoken, so they travel with the turn's direction instead.
    pub notes: Vec<String>,
}

impl CueParse {
    pub fn segments(&self) -> Vec<ScriptSegment> {
        segments_of(&self.canonical)
    }

    pub fn events(&self) -> Vec<VocalEvent> {
        self.segments()
            .into_iter()
            .filter_map(|segment| match segment {
                ScriptSegment::Event(event) => Some(event),
                ScriptSegment::Words(_) => None,
            })
            .collect()
    }

    /// The words alone, with every event removed.
    pub fn words(&self) -> String {
        words_of(&self.canonical)
    }

    /// A reaction says nothing and performs something: `[Laughter]`, `(applause)`.
    pub fn is_reaction(&self) -> bool {
        self.words().is_empty() && !self.events().is_empty()
    }
}

/// Read a writer's line into canonical form.
///
/// Every `(…)`, `[…]`, and `*…*` group is examined. A group that names a vocal event becomes the
/// canonical token wherever it stood. A bracketed or starred group that does not is a note, never
/// spoken. A parenthesised group that is neither is prose - "(and I mean it)" is something people
/// say - and stays in the words.
pub fn normalize_cues(text: &str) -> CueParse {
    let mut canonical = String::with_capacity(text.len());
    let mut notes = Vec::new();
    let mut rest = text;
    while let Some((open_at, open)) = rest
        .char_indices()
        .find(|(_, character)| matches!(character, '(' | '[' | '*'))
    {
        let close = match open {
            '(' => ')',
            '[' => ']',
            _ => '*',
        };
        let after_open = &rest[open_at + open.len_utf8()..];
        let Some(close_at) = after_open.find(close) else {
            break;
        };
        let inner = &after_open[..close_at];
        // A lone asterisk or an empty pair is punctuation, not a cue.
        if inner.trim().is_empty() || (open == '*' && inner.split_whitespace().count() > 4) {
            canonical.push_str(&rest[..open_at + open.len_utf8()]);
            rest = after_open;
            continue;
        }
        canonical.push_str(&rest[..open_at]);
        match VocalEvent::from_cue(inner) {
            Some(event) => {
                push_spaced(&mut canonical, &event.token());
            }
            None if open == '(' => {
                canonical.push(open);
                canonical.push_str(inner);
                canonical.push(close);
            }
            None => notes.push(inner.trim().to_string()),
        }
        rest = &after_open[close_at + close.len_utf8()..];
    }
    canonical.push_str(rest);
    CueParse {
        canonical: collapse_whitespace(&canonical),
        notes,
    }
}

/// Render a canonical line for one engine. Events the vocabulary can write are written its way;
/// the rest are removed and returned, so the caller can say which cues this voice will not
/// perform.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedLine {
    pub text: String,
    pub dropped: Vec<VocalEvent>,
}

pub fn render_for_vocabulary(canonical: &str, vocabulary: VocalVocabulary) -> RenderedLine {
    let mut text = String::with_capacity(canonical.len());
    let mut dropped = Vec::new();
    for segment in segments_of(canonical) {
        match segment {
            ScriptSegment::Words(words) => push_spaced(&mut text, &words),
            ScriptSegment::Event(event) => match vocabulary.render(event) {
                Some(token) => push_spaced(&mut text, &token),
                None => dropped.push(event),
            },
        }
    }
    RenderedLine {
        text: collapse_whitespace(&text),
        dropped,
    }
}

/// Split canonical text into words and events.
pub fn segments_of(canonical: &str) -> Vec<ScriptSegment> {
    let mut segments = Vec::new();
    let mut words = String::new();
    let mut rest = canonical;
    while let Some(open_at) = rest.find('(') {
        let after_open = &rest[open_at + 1..];
        let Some(close_at) = after_open.find(')') else {
            break;
        };
        let inner = &after_open[..close_at];
        match VocalEvent::from_cue(inner) {
            Some(event) if inner == event.canonical() => {
                words.push_str(&rest[..open_at]);
                let trimmed = collapse_whitespace(&words);
                if !trimmed.is_empty() {
                    segments.push(ScriptSegment::Words(trimmed));
                }
                words.clear();
                segments.push(ScriptSegment::Event(event));
            }
            _ => {
                words.push_str(&rest[..open_at + 1 + close_at + 1]);
            }
        }
        rest = &after_open[close_at + 1..];
    }
    words.push_str(rest);
    let trimmed = collapse_whitespace(&words);
    if !trimmed.is_empty() {
        segments.push(ScriptSegment::Words(trimmed));
    }
    segments
}

/// The words of a canonical line with its events removed.
pub fn words_of(canonical: &str) -> String {
    render_for_vocabulary(canonical, VocalVocabulary::None).text
}

/// The events of a canonical line, in order.
pub fn events_of(canonical: &str) -> Vec<VocalEvent> {
    segments_of(canonical)
        .into_iter()
        .filter_map(|segment| match segment {
            ScriptSegment::Event(event) => Some(event),
            ScriptSegment::Words(_) => None,
        })
        .collect()
}

fn push_spaced(target: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    if !target.is_empty() && !target.ends_with(char::is_whitespace) {
        target.push(' ');
    }
    target.push_str(piece);
}

fn collapse_whitespace(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.trim().chars() {
        if character.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space {
            // No space before closing punctuation: "word ." reads as a typo the writer never made.
            if !matches!(character, '.' | ',' | '!' | '?' | ';' | ':') {
                collapsed.push(' ');
            }
            pending_space = false;
        }
        collapsed.push(character);
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_laugh_becomes_one_token() {
        for cue in [
            "(laughs)",
            "[Laughter]",
            "*laughing*",
            "( LOL )",
            "[laughs out loud]",
        ] {
            let parsed = normalize_cues(&format!("{cue} Good evening."));
            assert_eq!(parsed.canonical, "(laugh) Good evening.", "{cue}");
            assert_eq!(parsed.events(), vec![VocalEvent::Laugh]);
        }
    }

    #[test]
    fn a_cue_in_the_middle_of_a_line_stays_where_it_was() {
        let parsed = normalize_cues("You cannot be serious. [chuckles] Not at all.");
        assert_eq!(
            parsed.canonical,
            "You cannot be serious. (chuckle) Not at all."
        );
        assert_eq!(parsed.words(), "You cannot be serious. Not at all.");
    }

    #[test]
    fn a_reaction_has_events_and_no_words() {
        let parsed = normalize_cues("[Laughter] [Applause]");
        assert_eq!(parsed.canonical, "(laugh) (applause)");
        assert!(parsed.is_reaction());
        assert_eq!(parsed.words(), "");
    }

    #[test]
    fn a_bracketed_note_is_never_spoken_and_is_kept() {
        let parsed = normalize_cues("[SFX: door slams] Who is there? *beat* Hello?");
        assert_eq!(parsed.canonical, "Who is there? Hello?");
        assert_eq!(parsed.notes, vec!["SFX: door slams", "beat"]);
    }

    #[test]
    fn parenthesised_prose_stays_prose() {
        let parsed = normalize_cues("I said (and I mean it) never again.");
        assert_eq!(parsed.canonical, "I said (and I mean it) never again.");
        assert!(parsed.events().is_empty());
    }

    #[test]
    fn an_unclosed_bracket_is_left_alone() {
        let parsed = normalize_cues("Well [laughs I suppose.");
        assert_eq!(parsed.canonical, "Well [laughs I suppose.");
    }

    #[test]
    fn rendering_writes_the_engine_vocabulary_or_drops_the_cue() {
        let canonical = "(laugh) Same, fridge. (sigh) We're all doing our best.";
        let breeze = render_for_vocabulary(canonical, VocalVocabulary::Parenthesis);
        assert_eq!(breeze.text, canonical);
        assert!(breeze.dropped.is_empty());
        let turbo = render_for_vocabulary(canonical, VocalVocabulary::Bracket);
        assert_eq!(
            turbo.text,
            "[laugh] Same, fridge. [sigh] We're all doing our best."
        );
        let kokoro = render_for_vocabulary(canonical, VocalVocabulary::None);
        assert_eq!(kokoro.text, "Same, fridge. We're all doing our best.");
        assert_eq!(kokoro.dropped, vec![VocalEvent::Laugh, VocalEvent::Sigh]);
    }

    #[test]
    fn a_reaction_rendered_for_a_voice_without_events_is_empty() {
        let rendered = render_for_vocabulary("(laugh) (laugh)", VocalVocabulary::None);
        assert_eq!(rendered.text, "");
        assert_eq!(rendered.dropped.len(), 2);
    }

    #[test]
    fn canonical_tokens_round_trip_through_segments() {
        let segments = segments_of("Hello (laugh) there (clears throat) friend");
        assert_eq!(
            segments,
            vec![
                ScriptSegment::Words("Hello".into()),
                ScriptSegment::Event(VocalEvent::Laugh),
                ScriptSegment::Words("there".into()),
                ScriptSegment::Event(VocalEvent::ClearsThroat),
                ScriptSegment::Words("friend".into()),
            ]
        );
    }

    #[test]
    fn vocabulary_names_are_stable() {
        for vocabulary in [
            VocalVocabulary::None,
            VocalVocabulary::Parenthesis,
            VocalVocabulary::Bracket,
        ] {
            assert_eq!(
                VocalVocabulary::parse(vocabulary.as_str()),
                Some(vocabulary)
            );
        }
        assert_eq!(VocalVocabulary::parse("smoke signals"), None);
    }
}
