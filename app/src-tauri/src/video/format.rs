//! Show formats: the reusable shape of a series.
//!
//! Everything before this slice makes one excellent episode. A format is what makes the second one
//! cheap. It holds the decisions that do not change between episodes - who is in the cast, how they
//! are pronounced, how the conversation is timed, what the captions and canvas look like, how loud
//! the master is, and what the opening and closing sound like - so a new episode starts as a brief
//! rather than as a hundred re-entered choices.
//!
//! Instantiation copies. An episode never reads back through its format at render time, so editing
//! a format cannot retroactively change an episode that already shipped, and an episode rendered
//! next year reproduces what it was rendered from today. The format origin recorded on the manifest
//! is provenance - it says where these values came from - not a live link.

use super::cast::CastMember;
use super::contracts::{
    validate_identifier, validate_nonempty, validate_timestamp_text, AudioMix, CanvasMode,
    CanvasSpec, CaptionPresetId, LayoutPlan, LengthTarget, Microseconds, NormalizedRect,
    RationalFrameRate, Validate, VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
    MAX_TIMELINE_DURATION_US,
};
use super::lexicon::LexiconEntry;
use super::performance::PerformanceClock;
use super::score::{CueAnchor, CueRole, MusicCue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_SHOW_FORMATS: usize = 64;
pub const MAX_SHOW_NOTES_STYLE_BYTES: usize = 2_000;

/// A cue the format supplies for every episode, before there is a script to anchor it to.
///
/// The anchor is deliberately absent: an opening belongs to whatever the first line turns out to
/// be, and a closing to whatever the last one is. Both are resolved when the episode has a script.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CueTemplate {
    pub id: String,
    pub role: CueRole,
    pub target_duration_us: Microseconds,
    pub direction: String,
    pub gain_db_milli: i32,
    pub fade_in_us: Microseconds,
    pub fade_out_us: Microseconds,
}

impl CueTemplate {
    /// Turn this template into a real cue against a written script.
    pub fn materialize(&self, anchor: CueAnchor, created_at: &str) -> VideoResult<MusicCue> {
        let cue = MusicCue {
            id: self.id.clone(),
            role: self.role,
            anchor,
            target_duration_us: self.target_duration_us,
            direction: self.direction.clone(),
            source_asset_id: None,
            track_id: None,
            gain_db_milli: self.gain_db_milli,
            fade_in_us: self.fade_in_us,
            fade_out_us: self.fade_out_us,
            created_at: created_at.to_string(),
        };
        cue.validate()?;
        Ok(cue)
    }
}

impl Validate for CueTemplate {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "show_format.cue.id")?;
        // Validating against a stand-in anchor proves the template's own values are usable before
        // any episode exists to anchor it to.
        let anchor = match self.role {
            CueRole::Outro => CueAnchor::AfterFinalTurn,
            _ => CueAnchor::Turn {
                turn_id: "template-probe".to_string(),
            },
        };
        self.materialize(anchor, "2000-01-01T00:00:00Z").map(|_| ())
    }
}

/// The reusable shape of a series.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShowFormat {
    pub id: String,
    pub name: String,
    /// Bumped on every save. An episode records which revision it inherited from, so provenance
    /// stays exact even as the format keeps evolving.
    pub revision: u32,
    pub cast: Vec<CastMember>,
    pub lexicon: Vec<LexiconEntry>,
    pub performance_clock: PerformanceClock,
    pub caption_preset_id: String,
    pub canvas_mode: CanvasMode,
    pub canvas: CanvasSpec,
    pub frame_rate: RationalFrameRate,
    pub target_lufs_milli: i32,
    pub true_peak_db_milli: i32,
    /// How long an episode of this show is meant to run. An episode is measured against it once
    /// performed, and one outside the tolerance is a quality finding until the writer accepts
    /// the length.
    pub target_duration_us: Microseconds,
    /// Slack either side of the target, in basis points of it. Two thousand is a fifth.
    #[serde(default = "default_duration_tolerance_bp")]
    pub duration_tolerance_bp: u32,
    #[serde(default)]
    pub opening: Option<CueTemplate>,
    #[serde(default)]
    pub closing: Option<CueTemplate>,
    #[serde(default)]
    pub show_notes_style: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A fifth either side: a thirty-second show may run twenty-four to thirty-six.
pub const DEFAULT_DURATION_TOLERANCE_BP: u32 = 2_000;

fn default_duration_tolerance_bp() -> u32 {
    DEFAULT_DURATION_TOLERANCE_BP
}

impl Validate for ShowFormat {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "show_format.id")?;
        LengthTarget {
            target_us: self.target_duration_us,
            tolerance_bp: self.duration_tolerance_bp,
        }
        .validate()?;
        validate_nonempty(&self.name, "show_format.name", 256)?;
        CaptionPresetId::parse(&self.caption_preset_id)?;
        self.canvas.validate()?;
        self.frame_rate.validate()?;
        self.performance_clock.validate()?;

        let cast_ids = self
            .cast
            .iter()
            .map(|member| member.id.as_str())
            .collect::<BTreeSet<_>>();
        super::cast::index_cast_by_name(&self.cast)?;
        super::lexicon::validate_lexicon(&self.lexicon, &cast_ids)?;

        // The mix targets are validated by building the mix an episode would actually inherit,
        // so a format cannot store a loudness target the renderer would later reject.
        AudioMix {
            target_lufs_milli: self.target_lufs_milli,
            true_peak_db_milli: self.true_peak_db_milli,
            tracks: Vec::new(),
        }
        .validate()?;

        if !(1..=MAX_TIMELINE_DURATION_US).contains(&self.target_duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidShowFormat,
                "an episode target length must be positive and no greater than six hours",
            )
            .at("show_format.target_duration_us"));
        }
        for (template, field) in [
            (&self.opening, "show_format.opening"),
            (&self.closing, "show_format.closing"),
        ] {
            if let Some(template) = template {
                template.validate()?;
                // An opening that resolves after the last line, or a closing that opens the show,
                // would be silently repositioned at instantiation.
                let expects_outro = field.ends_with("closing");
                if matches!(template.role, CueRole::Outro) != expects_outro {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidShowFormat,
                        "an opening cue cannot be an outro, and a closing cue must be",
                    )
                    .at(field));
                }
            }
        }
        if let Some(style) = &self.show_notes_style {
            validate_nonempty(
                style,
                "show_format.show_notes_style",
                MAX_SHOW_NOTES_STYLE_BYTES,
            )?;
        }
        validate_timestamp_text(&self.created_at, "show_format.created_at")?;
        validate_timestamp_text(&self.updated_at, "show_format.updated_at")?;
        Ok(())
    }
}

/// Where an episode's inherited values came from.
///
/// This is provenance, not a live link. Nothing reads back through it at render time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FormatOrigin {
    pub format_id: String,
    pub format_name: String,
    pub format_revision: u32,
    pub instantiated_at: String,
}

impl Validate for FormatOrigin {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.format_id, "format_origin.format_id")?;
        validate_nonempty(&self.format_name, "format_origin.format_name", 256)?;
        validate_timestamp_text(&self.instantiated_at, "format_origin.instantiated_at")?;
        Ok(())
    }
}

/// Build the starting manifest for a new episode of this show.
///
/// The result is a pristine draft: cast, pronunciation, timing, canvas, and mix are inherited, and
/// nothing else exists yet. Cues are deliberately absent, because an opening and a closing need a
/// script to anchor to; `materialize_format_cues` adds them once one is written.
pub fn instantiate_format(
    format: &ShowFormat,
    project_id: &str,
    episode_name: &str,
    created_at: &str,
) -> VideoResult<VideoProjectManifest> {
    format.validate()?;
    let mut manifest = VideoProjectManifest::new(
        project_id,
        episode_name,
        format.frame_rate,
        format.target_duration_us,
        LayoutPlan {
            mode: format.canvas_mode.clone(),
            canvas: format.canvas.clone(),
            safe_area: NormalizedRect {
                x_bp: 500,
                y_bp: 500,
                width_bp: 9_000,
                height_bp: 9_000,
            },
            background_rgba: [0, 0, 0, 255],
            elements: Vec::new(),
        },
        AudioMix {
            target_lufs_milli: format.target_lufs_milli,
            true_peak_db_milli: format.true_peak_db_milli,
            tracks: Vec::new(),
        },
        created_at,
    )?;
    manifest.cast = format.cast.clone();
    manifest.lexicon = format.lexicon.clone();
    manifest.performance_clock = format.performance_clock;
    manifest.length_target = Some(LengthTarget {
        target_us: format.target_duration_us,
        tolerance_bp: format.duration_tolerance_bp,
    });
    manifest.format_origin = Some(FormatOrigin {
        format_id: format.id.clone(),
        format_name: format.name.clone(),
        format_revision: format.revision,
        instantiated_at: created_at.to_string(),
    });
    manifest.validate_strict()?;
    Ok(manifest)
}

/// Resolve the format's opening and closing against a written script.
///
/// Returns the cues to add. An episode with no script gets none: there is nothing for an opening to
/// begin on or a closing to resolve after, and inventing an anchor would place music at a moment
/// the writer never chose.
pub fn materialize_format_cues(
    manifest: &VideoProjectManifest,
    opening: Option<&CueTemplate>,
    closing: Option<&CueTemplate>,
    created_at: &str,
) -> VideoResult<Vec<MusicCue>> {
    let Some(first_turn) = manifest.dialogue.first() else {
        return Ok(Vec::new());
    };
    let mut cues = Vec::new();
    if let Some(opening) = opening {
        cues.push(opening.materialize(
            CueAnchor::Turn {
                turn_id: first_turn.id.clone(),
            },
            created_at,
        )?);
    }
    if let Some(closing) = closing {
        cues.push(closing.materialize(CueAnchor::AfterFinalTurn, created_at)?);
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::cast::{CastDelivery, DialogueTurn};
    use crate::video::lexicon::{LexiconMatch, LexiconScope};

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn member(id: &str, name: &str, voice: &str) -> CastMember {
        CastMember {
            id: id.into(),
            name: name.into(),
            display_name: name.into(),
            voice_id: voice.into(),
            model_id: "hexgrad/Kokoro-82M".into(),
            language: "en-US".into(),
            delivery: CastDelivery::default(),
            consent_reference_id: None,
            persona: None,
            ensemble: 1,
            notes: None,
            created_at: NOW.into(),
        }
    }

    fn template(id: &str, role: CueRole) -> CueTemplate {
        CueTemplate {
            id: id.into(),
            role,
            target_duration_us: Microseconds(8_000_000),
            direction: "warm, restrained, low strings".into(),
            gain_db_milli: -6_000,
            fade_in_us: Microseconds(500_000),
            fade_out_us: Microseconds(1_500_000),
        }
    }

    fn format() -> ShowFormat {
        ShowFormat {
            id: "show-harmattan".into(),
            name: "The Harmattan Letters".into(),
            revision: 3,
            cast: vec![
                member("narrator", "NARRATOR", "af-heart"),
                member("adaeze", "ADAEZE", "af-bella"),
            ],
            lexicon: vec![LexiconEntry {
                id: "rule-adaeze".into(),
                scope: LexiconScope::Project,
                character_id: None,
                match_text: "Adaeze".into(),
                replacement: "Ah-DAH-eh-zeh".into(),
                matching: LexiconMatch::Word,
                notes: None,
                created_at: NOW.into(),
            }],
            performance_clock: PerformanceClock::default(),
            caption_preset_id: "podcast".into(),
            canvas_mode: CanvasMode::Portrait,
            canvas: CanvasSpec {
                width: 1080,
                height: 1920,
                pixel_aspect_numerator: 1,
                pixel_aspect_denominator: 1,
            },
            frame_rate: RationalFrameRate::FPS_30,
            target_lufs_milli: -16_000,
            true_peak_db_milli: -1_000,
            target_duration_us: Microseconds(600_000_000),
            duration_tolerance_bp: 2_000,
            opening: Some(template("cue-opening", CueRole::Sting)),
            closing: Some(template("cue-closing", CueRole::Outro)),
            show_notes_style: Some("Three short paragraphs, no bullet lists.".into()),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    fn turn(id: &str, order: u32, character: &str, text: &str) -> DialogueTurn {
        DialogueTurn {
            id: id.into(),
            scene_id: None,
            order,
            character_id: character.into(),
            text: text.into(),
            direction: None,
            source_line: order + 1,
            revision: 1,
        }
    }

    #[test]
    fn an_episode_inherits_the_shows_decisions() {
        let manifest = instantiate_format(&format(), "project-ep-1", "Episode 1", NOW).unwrap();
        assert_eq!(manifest.cast.len(), 2);
        assert_eq!(manifest.lexicon.len(), 1);
        assert_eq!(manifest.audio_mix.target_lufs_milli, -16_000);
        assert_eq!(manifest.layout.canvas.width, 1080);
        assert_eq!(manifest.timeline_duration_us, Microseconds(600_000_000));
        // A pristine draft: nothing has been written or rendered yet.
        assert!(manifest.dialogue.is_empty());
        assert!(manifest.music_cues.is_empty());
        assert!(manifest.source_assets.is_empty());
    }

    #[test]
    fn an_episode_records_which_format_revision_it_came_from() {
        let manifest = instantiate_format(&format(), "project-ep-1", "Episode 1", NOW).unwrap();
        let origin = manifest
            .format_origin
            .expect("the episode records its origin");
        assert_eq!(origin.format_id, "show-harmattan");
        assert_eq!(origin.format_revision, 3);
    }

    #[test]
    fn editing_a_format_cannot_reach_an_episode_that_already_exists() {
        let episode = instantiate_format(&format(), "project-ep-1", "Episode 1", NOW).unwrap();

        // The show recasts a character and drops the pronunciation rule.
        let mut revised = format();
        revised.revision = 4;
        revised.cast[1].voice_id = "af-nova".into();
        revised.lexicon.clear();
        revised.validate().unwrap();

        // The already-instantiated episode is untouched: instantiation copied, it did not link.
        assert_eq!(episode.cast[1].voice_id, "af-bella");
        assert_eq!(episode.lexicon.len(), 1);
        assert_eq!(
            episode
                .format_origin
                .as_ref()
                .map(|origin| origin.format_revision),
            Some(3)
        );

        // A new episode picks up the change.
        let next = instantiate_format(&revised, "project-ep-2", "Episode 2", NOW).unwrap();
        assert_eq!(next.cast[1].voice_id, "af-nova");
        assert!(next.lexicon.is_empty());
    }

    #[test]
    fn cues_are_anchored_to_the_script_rather_than_invented() {
        let format = format();
        let mut manifest = instantiate_format(&format, "project-ep-1", "Episode 1", NOW).unwrap();

        // With no script there is nothing for an opening to begin on or a closing to follow.
        assert!(materialize_format_cues(
            &manifest,
            format.opening.as_ref(),
            format.closing.as_ref(),
            NOW
        )
        .unwrap()
        .is_empty());

        manifest.dialogue = vec![
            turn("turn-a", 0, "narrator", "The harmattan came early."),
            turn("turn-b", 1, "adaeze", "You said you would come back."),
        ];
        let cues = materialize_format_cues(
            &manifest,
            format.opening.as_ref(),
            format.closing.as_ref(),
            NOW,
        )
        .unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(
            cues[0].anchor,
            CueAnchor::Turn {
                turn_id: "turn-a".into()
            }
        );
        assert_eq!(cues[1].anchor, CueAnchor::AfterFinalTurn);
        assert!(cues.iter().all(|cue| cue.needs_generation()));
    }

    #[test]
    fn an_opening_cannot_be_an_outro_and_a_closing_must_be() {
        let mut wrong_opening = format();
        wrong_opening.opening = Some(template("cue-opening", CueRole::Outro));
        assert_eq!(
            wrong_opening.validate().unwrap_err().code,
            VideoErrorCode::InvalidShowFormat
        );

        let mut wrong_closing = format();
        wrong_closing.closing = Some(template("cue-closing", CueRole::Bed));
        assert_eq!(
            wrong_closing.validate().unwrap_err().code,
            VideoErrorCode::InvalidShowFormat
        );
    }

    #[test]
    fn a_format_cannot_store_values_the_renderer_would_reject() {
        let mut loud = format();
        loud.target_lufs_milli = -2_000;
        assert_eq!(
            loud.validate().unwrap_err().code,
            VideoErrorCode::InvalidAudioMix
        );

        let mut unknown_captions = format();
        unknown_captions.caption_preset_id = "no-such-preset".into();
        assert!(unknown_captions.validate().is_err());

        let mut ambiguous_cast = format();
        ambiguous_cast.cast[1].name = "narrator".into();
        assert_eq!(
            ambiguous_cast.validate().unwrap_err().code,
            VideoErrorCode::InvalidCast
        );
    }

    #[test]
    fn a_format_rule_cannot_name_a_character_outside_its_cast() {
        let mut stray = format();
        stray.lexicon[0].scope = LexiconScope::Character;
        stray.lexicon[0].character_id = Some("emeka".into());
        assert_eq!(
            stray.validate().unwrap_err().code,
            VideoErrorCode::UnknownSpeaker
        );
    }

    #[test]
    fn an_episode_length_must_be_renderable() {
        let mut endless = format();
        endless.target_duration_us = Microseconds(0);
        assert_eq!(
            endless.validate().unwrap_err().code,
            VideoErrorCode::InvalidShowFormat
        );
    }
}
