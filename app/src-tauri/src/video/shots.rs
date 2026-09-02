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

/// One generated shot: what it shows, where it sits on the episode clock, and the frame it is
/// generated at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShotPlan {
    pub index: usize,
    pub prompt: String,
    /// Where this shot's first appearance begins. A shot may be shown again later.
    pub start_us: Microseconds,
    pub duration_us: Microseconds,
    pub width: u32,
    pub height: u32,
}

impl ShotPlan {
    /// Content address for this shot, so an unchanged shot is never regenerated. The frame is part
    /// of the address: a portrait show must not be served a landscape clip that happens to share a
    /// prompt.
    pub fn cache_key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"shot-v2");
        hasher.update([0x1f]);
        hasher.update(self.prompt.as_bytes());
        hasher.update([0x1f]);
        hasher.update(format!("{}x{}", self.width, self.height).as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// The frame a clip is generated at for a canvas of this shape.
///
/// A generator renders a fixed budget of pixels; the budget is spent in the episode's own aspect
/// so a portrait show gets portrait footage rather than the middle third of a landscape frame.
/// Every side is a multiple of sixteen, which is the generator's grid.
pub fn clip_canvas_for(canvas_width: u32, canvas_height: u32) -> (u32, u32) {
    if canvas_height > canvas_width {
        (480, 864)
    } else if canvas_height == canvas_width {
        (640, 640)
    } else {
        (864, 480)
    }
}

/// Shots a show's world implies when nobody wrote any.
///
/// Three views of one place: wide, close, and moving. Enough variety to read as coverage rather
/// than a loop, and few enough that an episode is not committed to an hour of generation because
/// its writer did not describe a shot.
pub fn default_shots_for_world(world: &str) -> Vec<String> {
    let world = world.trim().trim_end_matches('.');
    if world.is_empty() {
        return Vec::new();
    }
    vec![
        format!("Wide establishing shot of {world}, slow push in"),
        format!("Close detail inside {world}, shallow focus, soft light"),
        format!("Slow lateral drift across {world}, atmospheric haze, beams of light"),
    ]
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
///
/// `world` is the show's look, appended to every shot so the shots share one place; `canvas` is
/// the frame they are generated at.
pub fn plan_shots(
    descriptions: &[String],
    episode_duration_us: i64,
    style: &str,
    world: Option<&str>,
    canvas: (u32, u32),
) -> Vec<ShotPlan> {
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
    let world = world
        .map(str::trim)
        .map(|value| value.trim_end_matches('.'))
        .filter(|value| !value.is_empty());
    usable
        .into_iter()
        .enumerate()
        .map(|(index, description)| {
            let start = span * index as i64;
            let mut prompt = description.to_string();
            // A shot that already names the world is not told it twice.
            if let Some(world) = world {
                if !description.to_lowercase().contains(&world.to_lowercase()) {
                    prompt = format!("{prompt}, in {world}");
                }
            }
            if !style.trim().is_empty() {
                prompt = format!("{prompt}, {}", style.trim());
            }
            ShotPlan {
                index,
                prompt: clamp_prompt(&prompt),
                start_us: Microseconds(start),
                duration_us: Microseconds(CLIP_DURATION_US),
                width: canvas.0,
                height: canvas.1,
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
    fn a_portrait_show_gets_portrait_footage() {
        assert_eq!(clip_canvas_for(1080, 1920), (480, 864));
        assert_eq!(clip_canvas_for(1920, 1080), (864, 480));
        assert_eq!(clip_canvas_for(720, 720), (640, 640));
        let portrait = plan_shots(&described(&["a doorway"]), 6_000_000, "", None, (480, 864));
        let landscape = plan_shots(&described(&["a doorway"]), 6_000_000, "", None, (864, 480));
        // Same prompt, different frame, different clip.
        assert_ne!(portrait[0].cache_key(), landscape[0].cache_key());
    }

    #[test]
    fn the_world_is_added_to_a_shot_once_and_never_twice() {
        let world = "a small brick-wall comedy club";
        let shots = plan_shots(
            &described(&[
                "a microphone under a spotlight",
                "wide shot of a small brick-wall comedy club",
            ]),
            6_000_000,
            "film grain",
            Some(world),
            (480, 864),
        );
        assert_eq!(
            shots[0].prompt,
            "a microphone under a spotlight, in a small brick-wall comedy club, film grain"
        );
        assert_eq!(
            shots[1].prompt,
            "wide shot of a small brick-wall comedy club, film grain"
        );
    }

    #[test]
    fn a_world_implies_three_shots_and_nothing_implies_none() {
        assert_eq!(default_shots_for_world("a comedy club.").len(), 3);
        assert!(default_shots_for_world("  ").is_empty());
        assert!(default_shots_for_world("a comedy club")[0].contains("a comedy club"));
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
            None,
            (864, 480),
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
        let a = plan_shots(&descriptions, 6_000_000, "cinematic", None, (864, 480));
        let b = plan_shots(&descriptions, 6_000_000, "cinematic", None, (864, 480));
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
            None,
            (864, 480),
        );
        assert_eq!(shots.len(), 1);
        assert!(shots[0].prompt.starts_with("a quiet street"));
        assert!(plan_shots(
            &described(&["", " "]),
            6_000_000,
            "cinematic",
            None,
            (864, 480)
        )
        .is_empty());
    }

    #[test]
    fn more_descriptions_than_soundar_will_generate_are_capped() {
        let many = (0..40)
            .map(|i| format!("shot number {i}"))
            .collect::<Vec<_>>();
        assert_eq!(
            plan_shots(&many, 600_000_000, "cinematic", None, (864, 480)).len(),
            MAX_SHOTS
        );
    }

    #[test]
    fn a_long_description_is_trimmed_rather_than_handed_over_whole() {
        let long = "word ".repeat(400);
        let shots = plan_shots(
            &described(&[&long]),
            6_000_000,
            "cinematic",
            None,
            (864, 480),
        );
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
        assert!(plan_shots(&described(&["a street"]), 0, "cinematic", None, (864, 480)).is_empty());
        assert!(tile_shots(0, 10_000_000).is_empty());
        assert!(tile_shots(3, 0).is_empty());
    }
}
