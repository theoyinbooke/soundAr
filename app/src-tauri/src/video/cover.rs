//! Generated cover art for an episode that has no picture of its own.
//!
//! A script performed by voices produces sound and nothing to look at, and every video deliverable
//! needs a picture. Rather than refuse to package such an episode, soundAr draws one: a card built
//! from what the episode already knows about itself - its name and who is in it.
//!
//! The card is derived, not invented. The same episode always produces the same card, so a cover is
//! cacheable, reproducible from the manifest alone, and never silently different between two builds
//! of the same episode. It is also always marked as generated, so it is never mistaken for
//! artwork the user supplied.

use super::cast::CastMember;
use sha2::{Digest, Sha256};

/// Longest title drawn on a card. Past this a title stops being readable at a glance, and the
/// filter argument stops being a sane length.
pub const MAX_COVER_TITLE_CHARS: usize = 96;
/// Longest cast line. A large cast is summarised rather than run off the card.
pub const MAX_COVER_SUBTITLE_CHARS: usize = 72;
/// Cast members named individually before the line becomes a count.
pub const MAX_NAMED_CAST: usize = 4;

/// One card's colours. Chosen from a fixed set rather than computed, so every generated cover is a
/// deliberate combination that has been looked at rather than whatever a hash happened to produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoverPalette {
    pub background: &'static str,
    pub foreground: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
}

/// The palettes a generated cover may use. Each pairs a dark ground with text that clears a wide
/// contrast margin against it, because the card is read before it is admired.
pub const COVER_PALETTES: [CoverPalette; 6] = [
    CoverPalette {
        background: "0x1F2933",
        foreground: "0xF5F5F4",
        muted: "0x9AA5B1",
        accent: "0xE8B44A",
    },
    CoverPalette {
        background: "0x1B2A24",
        foreground: "0xF2F5F3",
        muted: "0x93A79C",
        accent: "0x6FCF97",
    },
    CoverPalette {
        background: "0x2A1F2D",
        foreground: "0xF6F2F7",
        muted: "0xAB9CB0",
        accent: "0xC084FC",
    },
    CoverPalette {
        background: "0x2B2118",
        foreground: "0xF7F3EE",
        muted: "0xB3A192",
        accent: "0xF59E6B",
    },
    CoverPalette {
        background: "0x18232E",
        foreground: "0xEFF4F8",
        muted: "0x92A4B3",
        accent: "0x60A5FA",
    },
    CoverPalette {
        background: "0x261C1C",
        foreground: "0xF7F1F1",
        muted: "0xB29A9A",
        accent: "0xF87171",
    },
];

/// Everything needed to draw one cover, and nothing about how it is drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverSpec {
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub subtitle: String,
    pub palette: CoverPalette,
}

impl CoverSpec {
    /// Content address for this exact card. Two episodes that would draw the same card share one
    /// generated file; changing a title or the cast changes the address, so the cover follows the
    /// episode rather than going stale against it.
    ///
    /// The address covers the drawing decisions, not only the inputs. Type sizes and the accent
    /// rule are derived from the canvas, so hashing the inputs alone would let a change to how a
    /// card is drawn keep serving the card drawn by the old rules, forever and silently.
    pub fn cache_key(&self) -> String {
        let mut hasher = Sha256::new();
        // Field separators, so a title ending in the subtitle's first word cannot collide with the
        // pair that actually reads that way.
        hasher.update(b"cover-v1");
        for field in [
            self.width.to_string().as_str(),
            self.height.to_string().as_str(),
            self.title.as_str(),
            self.subtitle.as_str(),
            self.palette.background,
            self.palette.foreground,
            self.palette.muted,
            self.palette.accent,
            self.title_font_size().to_string().as_str(),
            self.subtitle_font_size().to_string().as_str(),
            self.accent_height().to_string().as_str(),
        ] {
            hasher.update([0x1f]);
            hasher.update(field.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Title size that keeps a long name on the card. Derived from the canvas so the same card
    /// reads the same way at every resolution.
    ///
    /// The size stays a fraction of the canvas rather than being capped at a pixel count: a card
    /// is drawn once at full canvas size and scaled down for every preview, so a fixed ceiling
    /// makes the title shrink to nothing on a large canvas viewed small.
    pub fn title_font_size(&self) -> u32 {
        let base = (self.width / 20).max(28);
        // A long title needs smaller type, or it runs past the edge rather than wrapping - drawtext
        // does not wrap on its own.
        let crowding = (self.title.chars().count().max(1) as u32)
            .div_ceil(24)
            .max(1);
        (base / crowding).max(24)
    }

    pub fn subtitle_font_size(&self) -> u32 {
        (self.title_font_size() / 2).max(18)
    }

    /// Height of the accent rule along the top edge.
    pub fn accent_height(&self) -> u32 {
        (self.height / 135).clamp(4, 16)
    }
}

/// Truncate on a character boundary, and say that it was truncated rather than ending mid-word as
/// though the name simply stopped there.
fn clamp_text(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let kept = trimmed
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    format!("{}…", kept.trim_end())
}

/// The cast line for a card: who is in this episode, summarised once the list stops being readable.
pub fn cover_subtitle(cast: &[CastMember]) -> String {
    if cast.is_empty() {
        return String::new();
    }
    if cast.len() > MAX_NAMED_CAST {
        // A count is honest where a truncated list would imply the episode has only those voices.
        return format!("{} voices", cast.len());
    }
    clamp_text(
        &cast
            .iter()
            .map(|member| member.display_name.trim())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        MAX_COVER_SUBTITLE_CHARS,
    )
}

/// Pick the palette for an episode. Seeded by identity rather than by title, so renaming an episode
/// restyles its type without also changing its colour out from under the user.
pub fn cover_palette(seed: &str) -> CoverPalette {
    let digest = Sha256::digest(seed.as_bytes());
    COVER_PALETTES[usize::from(digest[0]) % COVER_PALETTES.len()]
}

/// Build the card for one episode.
pub fn cover_spec(
    project_id: &str,
    name: &str,
    cast: &[CastMember],
    width: u32,
    height: u32,
) -> CoverSpec {
    let title = clamp_text(name, MAX_COVER_TITLE_CHARS);
    CoverSpec {
        width,
        height,
        // An episode with no name still gets a card rather than a blank one.
        title: if title.is_empty() {
            "Untitled episode".to_string()
        } else {
            title
        },
        subtitle: cover_subtitle(cast),
        palette: cover_palette(project_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::cast::CastDelivery;

    fn member(id: &str, display_name: &str) -> CastMember {
        CastMember {
            id: id.to_string(),
            name: display_name.to_uppercase(),
            display_name: display_name.to_string(),
            voice_id: "af_heart".to_string(),
            model_id: "hexgrad/Kokoro-82M".to_string(),
            language: "en-US".to_string(),
            delivery: CastDelivery::default(),
            consent_reference_id: None,
            notes: None,
            created_at: "2026-08-30T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn the_same_episode_always_draws_the_same_card() {
        let cast = [member("a", "Mara"), member("b", "Tobi")];
        let first = cover_spec("project-1", "The Quiet Server", &cast, 1920, 1080);
        let second = cover_spec("project-1", "The Quiet Server", &cast, 1920, 1080);
        assert_eq!(first, second);
        assert_eq!(first.cache_key(), second.cache_key());
    }

    #[test]
    fn renaming_an_episode_redraws_the_card_but_keeps_its_colour() {
        let cast = [member("a", "Mara")];
        let before = cover_spec("project-1", "First name", &cast, 1920, 1080);
        let after = cover_spec("project-1", "Second name", &cast, 1920, 1080);
        // The card is regenerated, so the stale one is never reused for a renamed episode.
        assert_ne!(before.cache_key(), after.cache_key());
        // Identity, not the title, chooses the colour, so a rename does not restyle the show.
        assert_eq!(before.palette, after.palette);
    }

    #[test]
    fn recasting_an_episode_redraws_the_card() {
        let one = cover_spec("project-1", "Episode", &[member("a", "Mara")], 1920, 1080);
        let two = cover_spec(
            "project-1",
            "Episode",
            &[member("a", "Mara"), member("b", "Tobi")],
            1920,
            1080,
        );
        assert_ne!(one.cache_key(), two.cache_key());
        assert_eq!(two.subtitle, "Mara · Tobi");
    }

    #[test]
    fn every_palette_is_one_that_was_chosen_rather_than_computed() {
        // A hash picks between palettes; it never mixes its own colours, so no generated cover can
        // land on text that disappears into its background.
        for index in 0..64 {
            let palette = cover_palette(&format!("project-{index}"));
            assert!(COVER_PALETTES.contains(&palette));
            assert_ne!(palette.background, palette.foreground);
            assert_ne!(palette.background, palette.muted);
        }
    }

    #[test]
    fn a_large_cast_is_counted_rather_than_cut_off_mid_list() {
        let cast = (0..9)
            .map(|index| member(&format!("c{index}"), &format!("Voice {index}")))
            .collect::<Vec<_>>();
        // Listing four and stopping would read as though the episode had only four voices.
        assert_eq!(cover_subtitle(&cast), "9 voices");
    }

    #[test]
    fn an_unnamed_episode_still_gets_a_card() {
        let spec = cover_spec("project-1", "   ", &[], 1920, 1080);
        assert_eq!(spec.title, "Untitled episode");
        assert!(spec.subtitle.is_empty());
    }

    #[test]
    fn a_long_title_is_shortened_visibly_and_shrunk_to_fit() {
        let long =
            "A remarkably long episode title that keeps going well past anything that could \
                    be read at a glance on a card";
        let spec = cover_spec("project-1", long, &[], 1920, 1080);
        assert!(spec.title.chars().count() <= MAX_COVER_TITLE_CHARS);
        // Truncation is marked, so the title never looks like it simply ended there.
        assert!(spec.title.ends_with('…'));
        let short = cover_spec("project-1", "Short", &[], 1920, 1080);
        assert!(spec.title_font_size() < short.title_font_size());
        assert!(spec.title_font_size() >= 24);
    }

    #[test]
    fn title_type_stays_a_fraction_of_the_canvas_at_every_size() {
        // A card is drawn once at full size and scaled down for every preview, so type that is
        // capped at a pixel count disappears on a large canvas shown small.
        for (width, height) in [(1280_u32, 720_u32), (1920, 1080), (3840, 2160)] {
            let spec = cover_spec("project-1", "Close to Home", &[], width, height);
            let size = spec.title_font_size();
            assert!(
                size >= width / 32,
                "{width}x{height}: title {size} is too small"
            );
            assert!(
                size <= width / 8,
                "{width}x{height}: title {size} is too large"
            );
        }
    }

    #[test]
    fn changing_how_a_card_is_drawn_changes_its_address() {
        // The drawn pixels depend on derived type sizes as well as on the title. If the address
        // ignored them, changing the drawing rules would keep serving cards drawn by the old ones.
        let spec = cover_spec("project-1", "Close to Home", &[], 1920, 1080);
        let restyled = CoverSpec {
            title: format!("{} ", spec.title),
            ..spec.clone()
        };
        assert_ne!(spec.cache_key(), restyled.cache_key());

        let wider = CoverSpec {
            width: 2560,
            ..spec.clone()
        };
        assert_ne!(spec.title_font_size(), wider.title_font_size());
        assert_ne!(spec.cache_key(), wider.cache_key());
    }

    #[test]
    fn a_card_scales_with_its_canvas() {
        let small = cover_spec("project-1", "Episode", &[], 640, 360);
        let large = cover_spec("project-1", "Episode", &[], 3840, 2160);
        assert!(large.title_font_size() > small.title_font_size());
        assert!(large.accent_height() >= small.accent_height());
        // Two canvases are two different files, never one reused at the wrong size.
        assert_ne!(small.cache_key(), large.cache_key());
    }
}
