//! Shot planning for a generated episode.
//!
//! An episode performed by voices has sound and nothing to look at. A drawn cover card gives it a
//! picture; generated clips give it a picture that moves. Clips are expensive - on the order of a
//! minute of compute for under two seconds of footage - so an episode is never covered one-to-one.
//! It is covered by a handful of distinct shots, cut and repeated across the narration, which is
//! how b-roll has always worked.
//!
//! What each shot shows is derived from the episode itself, never invented here: its name, its
//! cast, and the words actually spoken during the span the shot covers.

use super::contracts::Microseconds;
use sha2::{Digest, Sha256};

/// Longest clip the generator produces at the settings soundAr uses.
pub const CLIP_DURATION_US: i64 = 1_583_333;
/// Fewest distinct shots worth generating. Below this the repetition is obvious.
pub const MIN_SHOTS: usize = 3;
/// Most shots soundAr will generate unasked. Each one costs about a minute, so an episode that
/// would want forty shots gets twelve and repeats them rather than running for most of an hour.
pub const MAX_SHOTS: usize = 12;
/// Longest prompt handed to the generator.
pub const MAX_SHOT_PROMPT_CHARS: usize = 400;

/// One generated shot: what it shows, and where it sits on the episode clock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShotPlan {
    pub index: usize,
    pub prompt: String,
    /// Where this shot's first appearance begins. A shot may be shown again later.
    pub start_us: Microseconds,
    pub duration_us: Microseconds,
}

impl ShotPlan {
    /// Content address for this shot, so an unchanged shot is never regenerated.
    pub fn cache_key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"shot-v1");
        hasher.update([0x1f]);
        hasher.update(self.prompt.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// How many distinct shots an episode of this length deserves.
///
/// Long enough to avoid obvious repetition, bounded so an episode cannot silently commit the
/// machine to an hour of generation.
pub fn shot_count_for(episode_duration_us: i64) -> usize {
    if episode_duration_us <= 0 {
        return 0;
    }
    // Roughly one distinct shot per five seconds, which repeats each shot about three times in a
    // minute-long episode - frequent enough to read as intentional cutting rather than as a loop.
    let wanted = (episode_duration_us / 5_000_000) as usize + 1;
    wanted.clamp(MIN_SHOTS, MAX_SHOTS)
}

/// Trim to a whole character boundary without running past the generator's prompt budget.
fn clamp_prompt(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SHOT_PROMPT_CHARS {
        return collapsed;
    }
    collapsed
        .chars()
        .take(MAX_SHOT_PROMPT_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Lay a set of authored shot descriptions across the episode clock.
///
/// The descriptions are written, never derived. An episode's name, cast and dialogue describe a
/// conversation; a shot description has to say what is on screen, and no amount of concatenating
/// the former produces the latter - it produces a sentence about a podcast, which a video model
/// renders as nothing recognisable. soundAr therefore places and content-addresses shots, and
/// leaves describing them to whoever is writing the show.
pub fn plan_shots(descriptions: &[String], episode_duration_us: i64, style: &str) -> Vec<ShotPlan> {
    let usable = descriptions
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .take(MAX_SHOTS)
        .collect::<Vec<_>>();
    if usable.is_empty() || episode_duration_us <= 0 {
        return Vec::new();
    }
    let span = episode_duration_us / usable.len() as i64;
    usable
        .into_iter()
        .enumerate()
        .map(|(index, description)| {
            let start = span * index as i64;
            let prompt = if style.trim().is_empty() {
                description.to_string()
            } else {
                format!("{description}, {}", style.trim())
            };
            ShotPlan {
                index,
                prompt: clamp_prompt(&prompt),
                start_us: Microseconds(start),
                duration_us: Microseconds(CLIP_DURATION_US),
            }
        })
        .collect()
}

/// Lay the generated shots across the whole episode, repeating the sequence until the clock is
/// covered.
///
/// Each placement names the shot it plays, so a caller can cut between a small number of generated
/// clips rather than generating one clip per second of episode.
pub fn tile_shots(
    shot_count: usize,
    episode_duration_us: i64,
) -> Vec<(usize, Microseconds, Microseconds)> {
    if shot_count == 0 || episode_duration_us <= 0 {
        return Vec::new();
    }
    let mut placements = Vec::new();
    let mut cursor = 0_i64;
    let mut index = 0_usize;
    while cursor < episode_duration_us {
        // The final placement is trimmed to the episode's end rather than overhanging it.
        let remaining = episode_duration_us - cursor;
        let duration = CLIP_DURATION_US.min(remaining);
        placements.push((
            index % shot_count,
            Microseconds(cursor),
            Microseconds(duration),
        ));
        cursor += duration;
        index += 1;
    }
    placements
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn an_episode_gets_more_shots_the_longer_it_runs_but_never_without_bound() {
        // A clip costs about a minute of compute, so an episode must not be able to commit the
        // machine to an unbounded run just by being long.
        assert_eq!(shot_count_for(0), 0);
        assert_eq!(shot_count_for(3_000_000), MIN_SHOTS);
        assert!(shot_count_for(60_000_000) > MIN_SHOTS);
        assert_eq!(shot_count_for(3_600_000_000), MAX_SHOTS);
    }

    #[test]
    fn authored_descriptions_are_placed_in_order_across_the_episode() {
        let shots = plan_shots(
            &described(&[
                "a lighthouse beam sweeping across black water at night",
                "an old man climbing a spiral iron staircase by lamplight",
                "waves breaking on rocks below a cliff at dawn",
            ]),
            9_000_000,
            "cinematic",
        );
        assert_eq!(shots.len(), 3);
        assert!(shots[0].prompt.starts_with("a lighthouse beam"));
        // The style is appended so an episode's shots look like one piece of work.
        assert!(shots[0].prompt.ends_with("cinematic"));
        assert_eq!(shots[0].start_us.0, 0);
        assert!(shots[1].start_us.0 > shots[0].start_us.0);
    }

    #[test]
    fn a_shot_is_never_generated_twice_and_different_shots_are_different_files() {
        let descriptions = described(&["a harbour at dusk", "a gull on a wet railing"]);
        let a = plan_shots(&descriptions, 6_000_000, "cinematic");
        let b = plan_shots(&descriptions, 6_000_000, "cinematic");
        assert_eq!(a, b);
        assert_eq!(a[0].cache_key(), b[0].cache_key());
        assert_ne!(a[0].cache_key(), a[1].cache_key());
    }

    #[test]
    fn blank_descriptions_are_dropped_rather_than_generated() {
        // An empty prompt would spend a minute of compute producing nothing recognisable.
        let shots = plan_shots(
            &described(&["", "   ", "a quiet street"]),
            6_000_000,
            "cinematic",
        );
        assert_eq!(shots.len(), 1);
        assert!(shots[0].prompt.starts_with("a quiet street"));
        assert!(plan_shots(&described(&["", " "]), 6_000_000, "cinematic").is_empty());
    }

    #[test]
    fn more_descriptions_than_soundar_will_generate_are_capped() {
        let many = (0..40)
            .map(|i| format!("shot number {i}"))
            .collect::<Vec<_>>();
        assert_eq!(plan_shots(&many, 600_000_000, "cinematic").len(), MAX_SHOTS);
    }

    #[test]
    fn a_long_description_is_trimmed_rather_than_handed_over_whole() {
        let long = "word ".repeat(400);
        let shots = plan_shots(&described(&[&long]), 6_000_000, "cinematic");
        assert!(shots[0].prompt.chars().count() <= MAX_SHOT_PROMPT_CHARS);
    }

    #[test]
    fn shots_tile_the_whole_episode_without_overhanging_its_end() {
        let placements = tile_shots(3, 10_000_000);
        assert!(!placements.is_empty());
        assert_eq!(placements[0].1 .0, 0);
        let last = placements.last().expect("a placement");
        assert_eq!(last.1 .0 + last.2 .0, 10_000_000);
        for window in placements.windows(2) {
            assert_eq!(window[0].1 .0 + window[0].2 .0, window[1].1 .0);
        }
        // Three clips cover ten seconds by repeating, rather than by generating seven more.
        assert!(placements.iter().all(|(index, _, _)| *index < 3));
        assert!(placements.len() > 3);
    }

    #[test]
    fn an_empty_episode_plans_nothing() {
        assert!(plan_shots(&described(&["a street"]), 0, "cinematic").is_empty());
        assert!(tile_shots(0, 10_000_000).is_empty());
        assert!(tile_shots(3, 0).is_empty());
    }
}
