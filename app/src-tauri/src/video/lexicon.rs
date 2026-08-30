//! Pronunciation rules applied to every line before a voice speaks it.
//!
//! A story invents names, and a speech model mispronounces them consistently. A lexicon fixes the
//! name once and every take of every character says it correctly, instead of the writer rewriting
//! the same word phonetically in forty places and losing the real spelling from the script.
//!
//! Two decisions make this reproducible rather than merely convenient.
//!
//! First, the project's lexicon is self-contained. Entries imported from the global lexicon are
//! snapshotted into the project with `Global` scope, so an episode rendered today reproduces
//! identically next year even if the machine's global lexicon has since changed.
//!
//! Second, a take records the fingerprint of the exact rules that produced it. Changing a rule
//! that affects a line makes that line's take stale and nothing else's, in the same way changing
//! the line's words does.

use super::contracts::{
    validate_identifier, validate_nonempty, validate_timestamp_text, Validate, VideoError,
    VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_LEXICON_ENTRIES: usize = 500;
pub const MAX_MATCH_TEXT_BYTES: usize = 200;
pub const MAX_REPLACEMENT_BYTES: usize = 400;

/// Which rules a lexicon entry belongs to, and therefore which lines it can change.
///
/// Precedence runs `Character` before `Project` before `Global`: the most specific rule wins, so
/// one character can pronounce a word their own way without changing it for everyone.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexiconScope {
    Character,
    Project,
    /// A snapshot of the machine's global lexicon taken when it was imported into this project.
    Global,
}

/// How a rule recognises the text it replaces.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LexiconMatch {
    /// Case-insensitive. The ordinary choice for a name that may be capitalised mid-sentence.
    Word,
    /// Case-sensitive. For an acronym or initialism whose capitalisation carries the meaning.
    Exact,
}

/// One pronunciation rule.
///
/// `replacement` is ordinary text, not a phoneme alphabet. soundAr's engines differ in what
/// notation they accept, and a rule that only works on one engine is worse than a respelling that
/// works everywhere.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LexiconEntry {
    pub id: String,
    pub scope: LexiconScope,
    /// Required for `Character` scope and rejected for every other scope.
    #[serde(default)]
    pub character_id: Option<String>,
    pub match_text: String,
    pub replacement: String,
    pub matching: LexiconMatch,
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: String,
}

impl Validate for LexiconEntry {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "lexicon.id")?;
        validate_nonempty(&self.match_text, "lexicon.match_text", MAX_MATCH_TEXT_BYTES)?;
        validate_nonempty(
            &self.replacement,
            "lexicon.replacement",
            MAX_REPLACEMENT_BYTES,
        )?;
        match (self.scope, &self.character_id) {
            (LexiconScope::Character, Some(character_id)) => {
                validate_identifier(character_id, "lexicon.character_id")?;
            }
            (LexiconScope::Character, None) => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidLexicon,
                    "a character-scoped rule must name the character it belongs to",
                )
                .at("lexicon.character_id"));
            }
            (_, Some(_)) => {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidLexicon,
                    "only a character-scoped rule may name a character",
                )
                .at("lexicon.character_id"));
            }
            (_, None) => {}
        }
        // A rule that rewrites a word to itself would apply forever without changing anything,
        // which reads as a broken rule rather than a deliberate one.
        if self.match_text.trim() == self.replacement.trim() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLexicon,
                "a rule must change the text it matches",
            )
            .at("lexicon.replacement"));
        }
        if let Some(notes) = &self.notes {
            if notes.len() > 2_000 {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidLexicon,
                    "lexicon notes are limited to 2000 bytes",
                )
                .at("lexicon.notes"));
            }
        }
        validate_timestamp_text(&self.created_at, "lexicon.created_at")?;
        Ok(())
    }
}

/// The result of rewriting one line for a voice.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LexiconApplication {
    /// Exactly what the engine is asked to say.
    pub spoken_text: String,
    /// Which rules actually fired, in the order they were applied. An entry that matched nothing
    /// is deliberately absent, so the record describes this line rather than the whole lexicon.
    pub applied_entry_ids: Vec<String>,
}

/// The rules that apply to one character's lines, in the order they are applied.
///
/// Ordering is precedence first, then longest match, then id. Longest-match matters: without it
/// a rule for `Adaeze` would consume the start of `Adaeze Nwosu` and the more specific rule for
/// the full name would never fire. The final id tie-break keeps the order stable across saves.
pub fn effective_entries<'a>(
    lexicon: &'a [LexiconEntry],
    character_id: &str,
) -> Vec<&'a LexiconEntry> {
    let mut entries = lexicon
        .iter()
        .filter(|entry| match entry.scope {
            LexiconScope::Character => entry.character_id.as_deref() == Some(character_id),
            LexiconScope::Project | LexiconScope::Global => true,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.scope
            .cmp(&right.scope)
            .then_with(|| right.match_text.len().cmp(&left.match_text.len()))
            .then_with(|| left.id.cmp(&right.id))
    });
    entries
}

/// Fingerprint the exact rules a take was produced under.
///
/// `None` means no rule applies to this character, which is also what every take recorded before
/// the lexicon existed carries, so those takes stay valid.
pub fn lexicon_fingerprint(entries: &[&LexiconEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.id.as_bytes());
        hasher.update([0x01]);
        hasher.update(entry.match_text.as_bytes());
        hasher.update([0x01]);
        hasher.update(entry.replacement.as_bytes());
        hasher.update([0x01]);
        hasher.update(match entry.matching {
            LexiconMatch::Word => b"word".as_slice(),
            LexiconMatch::Exact => b"exact".as_slice(),
        });
        hasher.update([0x02]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// The fingerprint of the rules that govern one character's lines.
pub fn fingerprint_for_character(lexicon: &[LexiconEntry], character_id: &str) -> Option<String> {
    lexicon_fingerprint(&effective_entries(lexicon, character_id))
}

/// Rewrite one line for speech.
///
/// Text produced by a rule is final: a later, lower-precedence rule cannot rewrite inside it. Rules
/// that cascade into each other would make the spoken result depend on evaluation order in a way
/// no one could predict from reading the lexicon.
pub fn apply_lexicon(text: &str, entries: &[&LexiconEntry]) -> LexiconApplication {
    // A segment is either still open to rewriting or already produced by a rule.
    let mut segments: Vec<(String, bool)> = vec![(text.to_string(), false)];
    let mut applied_entry_ids = Vec::new();

    for entry in entries {
        let mut next = Vec::with_capacity(segments.len());
        let mut fired = false;
        for (chunk, done) in segments {
            if done {
                next.push((chunk, true));
                continue;
            }
            let matches = find_matches(&chunk, &entry.match_text, entry.matching);
            if matches.is_empty() {
                next.push((chunk, false));
                continue;
            }
            fired = true;
            let mut cursor = 0usize;
            for (start, end) in matches {
                if start > cursor {
                    next.push((chunk[cursor..start].to_string(), false));
                }
                next.push((entry.replacement.clone(), true));
                cursor = end;
            }
            if cursor < chunk.len() {
                next.push((chunk[cursor..].to_string(), false));
            }
        }
        segments = next;
        if fired {
            applied_entry_ids.push(entry.id.clone());
        }
    }

    LexiconApplication {
        spoken_text: segments.into_iter().map(|(chunk, _)| chunk).collect(),
        applied_entry_ids,
    }
}

/// Non-overlapping match ranges, left to right.
///
/// A match must not sit inside a longer word. Without that, a rule for `Ada` would fire inside
/// `Adaeze` and the story would gain a mispronunciation rather than lose one.
fn find_matches(haystack: &str, needle: &str, matching: LexiconMatch) -> Vec<(usize, usize)> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
    while cursor < haystack.len() {
        let Some(end) = match_at(haystack, cursor, needle, matching) else {
            cursor = next_char_boundary(haystack, cursor);
            continue;
        };
        if is_word_bounded(haystack, cursor, end) {
            ranges.push((cursor, end));
            cursor = end;
        } else {
            cursor = next_char_boundary(haystack, cursor);
        }
    }
    ranges
}

/// The byte offset just past `needle` when it starts at `start`, or `None`.
fn match_at(haystack: &str, start: usize, needle: &str, matching: LexiconMatch) -> Option<usize> {
    if !haystack.is_char_boundary(start) {
        return None;
    }
    let mut candidates = haystack[start..].chars();
    let mut wanted = needle.chars();
    let mut consumed = start;
    loop {
        let Some(want) = wanted.next() else {
            return Some(consumed);
        };
        let found = candidates.next()?;
        let equal = match matching {
            // Per-character folding covers every name soundAr can reasonably be asked to say.
            // Multi-character foldings such as `ß` to `ss` deliberately do not match, rather than
            // matching approximately and mispronouncing a different word.
            LexiconMatch::Word => found.to_lowercase().eq(want.to_lowercase()),
            LexiconMatch::Exact => found == want,
        };
        if !equal {
            return None;
        }
        consumed += found.len_utf8();
    }
}

/// Whether the range stands alone rather than sitting inside a longer word.
fn is_word_bounded(haystack: &str, start: usize, end: usize) -> bool {
    let before = haystack[..start].chars().next_back();
    let after = haystack[end..].chars().next();
    !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '\''
}

fn next_char_boundary(haystack: &str, from: usize) -> usize {
    haystack[from..]
        .chars()
        .next()
        .map_or(haystack.len(), |value| from + value.len_utf8())
}

/// Validate a project's lexicon as a whole.
pub(crate) fn validate_lexicon(
    lexicon: &[LexiconEntry],
    character_ids: &std::collections::BTreeSet<&str>,
) -> VideoResult<()> {
    if lexicon.len() > MAX_LEXICON_ENTRIES {
        return Err(VideoError::new(
            VideoErrorCode::InvalidLexicon,
            format!("a project supports at most {MAX_LEXICON_ENTRIES} pronunciation rules"),
        )
        .at("lexicon"));
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    let mut seen_rules = std::collections::BTreeSet::new();
    for entry in lexicon {
        entry.validate()?;
        if !seen_ids.insert(entry.id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::DuplicateId,
                format!("duplicate identifier {}", entry.id),
            )
            .at("lexicon.id"));
        }
        if let Some(character_id) = entry.character_id.as_deref() {
            if !character_ids.contains(character_id) {
                return Err(VideoError::new(
                    VideoErrorCode::UnknownSpeaker,
                    "a pronunciation rule names a character who is not in the cast",
                )
                .at("lexicon.character_id"));
            }
        }
        // Two rules with the same scope, character, and match would make the spoken result depend
        // on which one happened to sort first.
        let rule_key = (
            entry.scope,
            entry.character_id.clone(),
            match entry.matching {
                LexiconMatch::Word => entry.match_text.to_lowercase(),
                LexiconMatch::Exact => entry.match_text.clone(),
            },
        );
        if !seen_rules.insert(rule_key) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLexicon,
                "two rules in the same scope match the same text",
            )
            .at("lexicon.match_text"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn entry(id: &str, scope: LexiconScope, match_text: &str, replacement: &str) -> LexiconEntry {
        LexiconEntry {
            id: id.into(),
            scope,
            character_id: match scope {
                LexiconScope::Character => Some("adaeze".into()),
                _ => None,
            },
            match_text: match_text.into(),
            replacement: replacement.into(),
            matching: LexiconMatch::Word,
            notes: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn apply(text: &str, lexicon: &[LexiconEntry], character: &str) -> LexiconApplication {
        apply_lexicon(text, &effective_entries(lexicon, character))
    }

    #[test]
    fn rewrites_a_name_wherever_it_appears() {
        let lexicon = vec![entry("e1", LexiconScope::Project, "Adaeze", "Ah-DAH-eh-zeh")];
        let applied = apply("Adaeze waited. Then adaeze left.", &lexicon, "narrator");
        assert_eq!(
            applied.spoken_text,
            "Ah-DAH-eh-zeh waited. Then Ah-DAH-eh-zeh left."
        );
        assert_eq!(applied.applied_entry_ids, vec!["e1"]);
    }

    #[test]
    fn never_fires_inside_a_longer_word() {
        let lexicon = vec![entry("e1", LexiconScope::Project, "Ada", "AY-dah")];
        let applied = apply("Adaeze met Ada.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "Adaeze met AY-dah.");
    }

    #[test]
    fn a_longer_rule_wins_over_a_shorter_one() {
        let lexicon = vec![
            entry("short", LexiconScope::Project, "Adaeze", "AH-dah"),
            entry(
                "long",
                LexiconScope::Project,
                "Adaeze Nwosu",
                "AH-dah NWOH-soo",
            ),
        ];
        let applied = apply("Adaeze Nwosu arrived.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "AH-dah NWOH-soo arrived.");
        assert_eq!(applied.applied_entry_ids, vec!["long"]);
    }

    #[test]
    fn a_character_rule_overrides_the_project_and_global_ones() {
        let lexicon = vec![
            entry("global", LexiconScope::Global, "Kano", "KAH-noh"),
            entry("project", LexiconScope::Project, "Kano", "KAH-no"),
            entry("character", LexiconScope::Character, "Kano", "KAH-naw"),
        ];
        assert_eq!(apply("Kano.", &lexicon, "adaeze").spoken_text, "KAH-naw.");
        // Another character never sees the character-scoped rule.
        assert_eq!(apply("Kano.", &lexicon, "narrator").spoken_text, "KAH-no.");
    }

    #[test]
    fn a_replacement_is_final_and_cannot_be_rewritten_again() {
        let lexicon = vec![
            entry("first", LexiconScope::Project, "Kano", "KAH-noh"),
            entry("second", LexiconScope::Global, "noh", "NO"),
        ];
        let applied = apply("Kano.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "KAH-noh.");
        assert_eq!(applied.applied_entry_ids, vec!["first"]);
    }

    #[test]
    fn an_exact_rule_respects_capitalisation() {
        let mut exact = entry("e1", LexiconScope::Project, "US", "U S");
        exact.matching = LexiconMatch::Exact;
        let lexicon = vec![exact];
        let applied = apply("The US told us.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "The U S told us.");
    }

    #[test]
    fn records_only_the_rules_that_actually_fired() {
        let lexicon = vec![
            entry("used", LexiconScope::Project, "Kano", "KAH-noh"),
            entry("unused", LexiconScope::Project, "Enugu", "EH-noo-goo"),
        ];
        let applied = apply("Kano.", &lexicon, "narrator");
        assert_eq!(applied.applied_entry_ids, vec!["used"]);
    }

    #[test]
    fn text_with_no_matching_rule_is_returned_unchanged() {
        let lexicon = vec![entry("e1", LexiconScope::Project, "Kano", "KAH-noh")];
        let applied = apply("Nothing to change here.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "Nothing to change here.");
        assert!(applied.applied_entry_ids.is_empty());
    }

    #[test]
    fn handles_multibyte_text_without_splitting_a_character() {
        let lexicon = vec![entry("e1", LexiconScope::Project, "Chiamaka", "chee-ah-MAH-kah")];
        let applied = apply("Ọ bụ Chiamaka — nwanne m.", &lexicon, "narrator");
        assert_eq!(applied.spoken_text, "Ọ bụ chee-ah-MAH-kah — nwanne m.");
    }

    #[test]
    fn the_fingerprint_changes_only_when_the_effective_rules_change() {
        let base = vec![entry("e1", LexiconScope::Project, "Kano", "KAH-noh")];
        let same = vec![entry("e1", LexiconScope::Project, "Kano", "KAH-noh")];
        let changed = vec![entry("e1", LexiconScope::Project, "Kano", "KAH-no")];
        assert_eq!(
            fingerprint_for_character(&base, "narrator"),
            fingerprint_for_character(&same, "narrator")
        );
        assert_ne!(
            fingerprint_for_character(&base, "narrator"),
            fingerprint_for_character(&changed, "narrator")
        );
    }

    #[test]
    fn a_character_rule_does_not_change_another_characters_fingerprint() {
        let lexicon = vec![entry("e1", LexiconScope::Character, "Kano", "KAH-naw")];
        assert!(fingerprint_for_character(&lexicon, "adaeze").is_some());
        assert_eq!(fingerprint_for_character(&lexicon, "narrator"), None);
    }

    #[test]
    fn an_empty_lexicon_has_no_fingerprint_so_older_takes_stay_valid() {
        assert_eq!(fingerprint_for_character(&[], "narrator"), None);
    }

    #[test]
    fn a_character_rule_must_name_a_character_in_the_cast() {
        let cast = BTreeSet::from(["narrator"]);
        let lexicon = vec![entry("e1", LexiconScope::Character, "Kano", "KAH-naw")];
        let error = validate_lexicon(&lexicon, &cast).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::UnknownSpeaker);
    }

    #[test]
    fn only_a_character_rule_may_name_a_character() {
        let mut stray = entry("e1", LexiconScope::Project, "Kano", "KAH-noh");
        stray.character_id = Some("adaeze".into());
        assert_eq!(
            stray.validate().unwrap_err().code,
            VideoErrorCode::InvalidLexicon
        );
    }

    #[test]
    fn a_rule_must_change_the_text_it_matches() {
        let noop = entry("e1", LexiconScope::Project, "Kano", "Kano");
        assert_eq!(
            noop.validate().unwrap_err().code,
            VideoErrorCode::InvalidLexicon
        );
    }

    #[test]
    fn two_rules_in_one_scope_cannot_match_the_same_text() {
        let cast = BTreeSet::from(["adaeze"]);
        let lexicon = vec![
            entry("e1", LexiconScope::Project, "Kano", "KAH-noh"),
            entry("e2", LexiconScope::Project, "kano", "KAH-no"),
        ];
        let error = validate_lexicon(&lexicon, &cast).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidLexicon);

        // The same word in two different scopes is the whole point of precedence.
        let scoped = vec![
            entry("e1", LexiconScope::Project, "Kano", "KAH-noh"),
            entry("e2", LexiconScope::Character, "Kano", "KAH-naw"),
        ];
        validate_lexicon(&scoped, &cast).unwrap();
    }
}
