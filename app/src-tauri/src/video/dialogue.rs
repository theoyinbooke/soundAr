//! Applying a cast and a written script to a durable project manifest.
//!
//! The product promise this module has to keep is narrow and load-bearing: rewriting one line of
//! a script must re-read one line. Everything here exists to make that true.
//!
//! A turn's identity is its content, not its position. When a script is re-applied, every turn
//! whose character and words are unchanged keeps the identifier it already had, so the narration
//! take bound to it stays valid. Only a turn whose words actually changed is minted fresh, and
//! only that turn's take is dropped. Reordering a scene therefore costs nothing, and fixing a
//! typo on line 40 of a 200-line script re-renders exactly one line.

use super::cast::{parse_dialogue_script, CastMember, DialogueTurn};
use super::contracts::{
    RevisionStage, Validate, VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
};
use super::performance::{derive_turn_beats, BeatSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DialogueScriptRequest {
    /// The complete cast for this project. Applying a script replaces the cast wholesale so the
    /// script and the characters who can speak it are always committed together.
    pub cast: Vec<CastMember>,
    pub script: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedDialogueScript {
    pub manifest: VideoProjectManifest,
    pub changed_paths: Vec<String>,
    pub invalidated_stages: BTreeSet<RevisionStage>,
    /// Turns whose words survived unchanged, and whose existing takes therefore survive with them.
    pub retained_turn_ids: Vec<String>,
    pub new_turn_ids: Vec<String>,
    /// Takes dropped because the words they were asked to speak no longer exist in the script.
    pub dropped_binding_ids: Vec<String>,
}

/// Apply a cast and script to a manifest without advancing its commit metadata.
///
/// Like `apply_timeline_edit`, this is a pure manifest transformation. The caller owns the
/// version check and the durable compare-and-swap commit.
pub fn apply_dialogue_script(
    manifest: &VideoProjectManifest,
    request: &DialogueScriptRequest,
) -> VideoResult<AppliedDialogueScript> {
    manifest.validate_strict()?;

    // Parsing before mutating means a script with an unknown speaker or an unclosed direction is
    // reported against its source line while the project is still untouched.
    let parsed = parse_dialogue_script(&request.script, &request.cast)?;

    // Index the turns already in the project by what they say. A turn is reusable at most once,
    // so a script that repeats an identical line keeps both of its takes in written order.
    let mut reusable: BTreeMap<String, VecDeque<&DialogueTurn>> = BTreeMap::new();
    for turn in &manifest.dialogue {
        reusable
            .entry(turn_identity(&turn.character_id, &turn.text))
            .or_default()
            .push_back(turn);
    }

    let mut turns = Vec::with_capacity(parsed.len());
    let mut retained_turn_ids = Vec::new();
    let mut new_turn_ids = Vec::new();
    let mut used_ids = BTreeSet::new();

    for (index, candidate) in parsed.iter().enumerate() {
        let order = u32::try_from(index).map_err(|_| {
            VideoError::new(
                VideoErrorCode::InvalidDialogue,
                "the script has more turns than soundAr can order",
            )
            .at("dialogue.order")
        })?;
        let identity = turn_identity(&candidate.character_id, &candidate.text);
        let existing = reusable
            .get_mut(&identity)
            .and_then(|queue| queue.pop_front());

        let (id, scene_id, revision) = match existing {
            Some(previous) => {
                retained_turn_ids.push(previous.id.clone());
                (
                    previous.id.clone(),
                    previous.scene_id.clone(),
                    previous.revision,
                )
            }
            None => {
                let id = mint_turn_id(order, &identity, &used_ids);
                new_turn_ids.push(id.clone());
                (id, None, 1)
            }
        };
        used_ids.insert(id.clone());

        let mut turn = DialogueTurn {
            id,
            scene_id,
            order,
            character_id: candidate.character_id.clone(),
            text: candidate.text.clone(),
            direction: candidate.direction.clone(),
            source_line: candidate.source_line,
            revision,
        };
        // A retained turn advances its revision only when something about it actually changed.
        // Bumping unconditionally would make re-applying an unchanged script look like an edit,
        // which would discard cached work and break idempotent replay after a crash.
        if let Some(previous) = existing {
            if differs_in_content(previous, &turn) {
                turn.revision = previous.revision.saturating_add(1);
            }
        }
        turn.validate()?;
        turns.push(turn);
    }

    let surviving_turn_ids = turns
        .iter()
        .map(|turn| turn.id.clone())
        .collect::<BTreeSet<_>>();
    let mut dropped_binding_ids = Vec::new();
    let mut narration_bindings = Vec::with_capacity(manifest.narration_bindings.len());
    for binding in &manifest.narration_bindings {
        match binding.turn_id.as_deref() {
            // A take whose turn no longer exists is stale by definition: the words it speaks are
            // not in the script any more. Keeping it would leave the manifest unvalidatable.
            Some(turn_id) if !surviving_turn_ids.contains(turn_id) => {
                dropped_binding_ids.push(binding.id.clone());
            }
            _ => narration_bindings.push(binding.clone()),
        }
    }

    // An explicit beat is the writer's decision about one line. It survives every edit to the
    // rest of the script because its turn keeps its identifier; it is discarded only when the
    // line it belongs to is gone. Derived beats are always recomputed from the new script.
    let surviving_overrides = manifest
        .turn_beats
        .iter()
        .filter(|beat| matches!(beat.source, BeatSource::Explicit))
        .filter(|beat| surviving_turn_ids.contains(&beat.turn_id))
        .cloned()
        .collect::<Vec<_>>();

    let mut applied = manifest.clone();
    applied.cast = request.cast.clone();
    applied.turn_beats =
        derive_turn_beats(&turns, &manifest.performance_clock, &surviving_overrides)?;
    applied.dialogue = turns;
    applied.narration_bindings = narration_bindings;
    // A dropped take leaves its clip pointing at narration the script no longer contains.
    for track in &mut applied.tracks {
        for clip in &mut track.clips {
            if let Some(turn_id) = clip.turn_id.as_deref() {
                if !surviving_turn_ids.contains(turn_id) {
                    clip.turn_id = None;
                }
            }
        }
    }
    applied.validate_strict()?;

    if applied.revision != manifest.revision
        || applied.revision_history != manifest.revision_history
        || applied.updated_at != manifest.updated_at
    {
        return Err(VideoError::new(
            VideoErrorCode::InvalidRevision,
            "applying a script may not advance commit metadata",
        )
        .at("revision"));
    }

    let changed_paths = super::manifest_changed_paths(manifest, &applied).map_err(|error| {
        VideoError::new(
            VideoErrorCode::InvalidRevision,
            format!("canonical manifest diff failed: {error}"),
        )
        .at("manifest")
    })?;
    if changed_paths.is_empty() {
        return Err(VideoError::new(
            VideoErrorCode::InvalidRevision,
            "the requested cast and script are already applied to this project",
        )
        .at("script"));
    }
    let invalidated_stages = super::invalidated_stages_for_manifest_changes(&changed_paths);

    Ok(AppliedDialogueScript {
        manifest: applied,
        changed_paths: changed_paths.into_iter().collect(),
        invalidated_stages,
        retained_turn_ids,
        new_turn_ids,
        dropped_binding_ids,
    })
}

/// Whether a retained turn changed in any way a reader or a renderer would notice. `revision` is
/// excluded because it is the value being decided.
fn differs_in_content(previous: &DialogueTurn, next: &DialogueTurn) -> bool {
    previous.order != next.order
        || previous.scene_id != next.scene_id
        || previous.direction != next.direction
        || previous.source_line != next.source_line
}

/// What makes two turns the same turn: the same character saying the same words. A changed stage
/// direction steers performance without changing the words, so it deliberately does not mint a new
/// turn or discard a valid take.
fn turn_identity(character_id: &str, text: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{character_id}\u{1}{text}")))
}

/// Mint a content-derived identifier so the same script applied twice produces the same project.
/// A `Date`- or counter-derived id would make an idempotent replay look like a change.
fn mint_turn_id(order: u32, identity: &str, used: &BTreeSet<String>) -> String {
    let short = &identity[..12];
    let base = format!("turn-{order:04}-{short}");
    if !used.contains(&base) {
        return base;
    }
    // Collision is effectively unreachable, but a duplicate identifier would be a validation
    // failure rather than a wrong answer, so resolve it deterministically instead of trusting luck.
    (1u32..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !used.contains(candidate))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::cast::CastDelivery;
    use crate::video::contracts::{
        AudioMix, AudioMixTrack, CanvasMode, CanvasSpec, LayoutPlan, Microseconds, NormalizedRect,
        RationalFrameRate,
    };

    fn timestamp() -> String {
        "2026-01-01T00:00:00Z".to_string()
    }

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
            created_at: timestamp(),
        }
    }

    fn cast() -> Vec<CastMember> {
        vec![
            member("narrator", "NARRATOR", "af-heart"),
            member("adaeze", "ADAEZE", "af-bella"),
        ]
    }

    fn project() -> VideoProjectManifest {
        VideoProjectManifest::new(
            "project-1",
            "Story episode",
            RationalFrameRate::FPS_30000_1001,
            Microseconds(3_000_000),
            LayoutPlan {
                mode: CanvasMode::Portrait,
                canvas: CanvasSpec {
                    width: 1080,
                    height: 1920,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: NormalizedRect {
                    x_bp: 500,
                    y_bp: 500,
                    width_bp: 9_000,
                    height_bp: 9_000,
                },
                background_rgba: [245, 245, 244, 255],
                elements: vec![],
            },
            AudioMix {
                target_lufs_milli: -16_000,
                true_peak_db_milli: -1_000,
                tracks: Vec::<AudioMixTrack>::new(),
            },
            timestamp(),
        )
        .unwrap()
    }

    const SCRIPT: &str = "NARRATOR: The harmattan came early.\n\nADAEZE: (quiet) You said you would come back.\n\nNARRATOR: She did not answer.\n";

    fn apply(manifest: &VideoProjectManifest, script: &str) -> AppliedDialogueScript {
        apply_dialogue_script(
            manifest,
            &DialogueScriptRequest {
                cast: cast(),
                script: script.into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn applies_a_script_as_ordered_turns() {
        let applied = apply(&project(), SCRIPT);
        let turns = &applied.manifest.dialogue;
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].character_id, "narrator");
        assert_eq!(turns[1].character_id, "adaeze");
        assert_eq!(turns[1].direction.as_deref(), Some("quiet"));
        assert_eq!(
            turns.iter().map(|turn| turn.order).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(applied.new_turn_ids.len(), 3);
        assert!(applied.retained_turn_ids.is_empty());
        assert_eq!(applied.manifest.cast.len(), 2);
    }

    #[test]
    fn the_same_script_applied_twice_produces_the_same_turn_ids() {
        let first = apply(&project(), SCRIPT);
        let second = apply(&project(), SCRIPT);
        assert_eq!(
            first
                .manifest
                .dialogue
                .iter()
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>(),
            second
                .manifest
                .dialogue
                .iter()
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn editing_one_line_mints_only_that_turn() {
        let first = apply(&project(), SCRIPT);
        let edited = SCRIPT.replace("She did not answer.", "She said nothing at all.");
        let second = apply(&first.manifest, &edited);

        assert_eq!(second.new_turn_ids.len(), 1);
        assert_eq!(second.retained_turn_ids.len(), 2);
        // The untouched turns keep the exact identifiers their takes are bound to.
        assert_eq!(
            second.retained_turn_ids,
            vec![
                first.manifest.dialogue[0].id.clone(),
                first.manifest.dialogue[1].id.clone()
            ]
        );
        assert_ne!(
            second.manifest.dialogue[2].id,
            first.manifest.dialogue[2].id
        );
    }

    #[test]
    fn reordering_turns_preserves_every_identifier() {
        let first = apply(&project(), SCRIPT);
        let reordered = "ADAEZE: (quiet) You said you would come back.\n\nNARRATOR: The harmattan came early.\n\nNARRATOR: She did not answer.\n";
        let second = apply(&first.manifest, reordered);
        assert!(second.new_turn_ids.is_empty());
        assert_eq!(second.retained_turn_ids.len(), 3);
        assert_eq!(
            second.manifest.dialogue[0].id,
            first.manifest.dialogue[1].id
        );
        assert_eq!(second.manifest.dialogue[0].order, 0);
    }

    #[test]
    fn a_repeated_line_keeps_both_of_its_turns() {
        let repeated = "NARRATOR: She waited.\n\nNARRATOR: She waited.\n";
        let first = apply(&project(), repeated);
        assert_eq!(first.manifest.dialogue.len(), 2);
        assert_ne!(first.manifest.dialogue[0].id, first.manifest.dialogue[1].id);
        // Appending a third identical line must reuse both existing turns rather than renaming
        // them, so the two takes already rendered for this line survive.
        let extended = "NARRATOR: She waited.\n\nNARRATOR: She waited.\n\nNARRATOR: She waited.\n";
        let second = apply(&first.manifest, extended);
        assert_eq!(second.retained_turn_ids.len(), 2);
        assert_eq!(second.new_turn_ids.len(), 1);
        assert_eq!(
            second.manifest.dialogue[0].id,
            first.manifest.dialogue[0].id
        );
        assert_eq!(
            second.manifest.dialogue[1].id,
            first.manifest.dialogue[1].id
        );
    }

    #[test]
    fn changing_only_a_direction_keeps_the_turn_and_its_take() {
        let first = apply(
            &project(),
            "ADAEZE: (quiet) You said you would come back.\n",
        );
        let second = apply(
            &first.manifest,
            "ADAEZE: (furious) You said you would come back.\n",
        );
        assert_eq!(second.retained_turn_ids.len(), 1);
        assert!(second.new_turn_ids.is_empty());
        assert_eq!(
            second.manifest.dialogue[0].direction.as_deref(),
            Some("furious")
        );
    }

    #[test]
    fn reapplying_an_unchanged_script_is_rejected_as_no_change() {
        let first = apply(&project(), SCRIPT);
        let error = apply_dialogue_script(
            &first.manifest,
            &DialogueScriptRequest {
                cast: cast(),
                script: SCRIPT.into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidRevision);
    }

    #[test]
    fn applying_a_script_derives_a_beat_for_every_turn() {
        let applied = apply(&project(), SCRIPT);
        assert_eq!(applied.manifest.turn_beats.len(), 3);
        assert_eq!(
            applied.manifest.turn_beats[0].lead_in_us,
            Microseconds::ZERO,
            "the first turn waits for nothing"
        );
        assert!(applied
            .manifest
            .turn_beats
            .iter()
            .all(|beat| matches!(beat.source, BeatSource::Derived)));
    }

    /// Replace one derived beat with a deliberate, writer-chosen pause.
    fn hold_beat(
        manifest: &VideoProjectManifest,
        turn_id: &str,
        lead_in_us: i64,
    ) -> VideoProjectManifest {
        use crate::video::performance::TurnBeat;
        let mut held = manifest.clone();
        held.turn_beats = held
            .turn_beats
            .into_iter()
            .map(|beat| {
                if beat.turn_id == turn_id {
                    TurnBeat {
                        turn_id: beat.turn_id,
                        lead_in_us: Microseconds(lead_in_us),
                        overlap_us: Microseconds::ZERO,
                        source: BeatSource::Explicit,
                    }
                } else {
                    beat
                }
            })
            .collect();
        held
    }

    #[test]
    fn an_explicit_beat_survives_an_edit_to_a_different_line() {
        let first = apply(&project(), SCRIPT);
        let held_turn = first.manifest.dialogue[1].id.clone();
        let with_override = hold_beat(&first.manifest, &held_turn, 2_500_000);
        with_override.validate_strict().unwrap();

        // Rewriting the third line must not disturb the deliberate pause on the second.
        let edited = apply(
            &with_override,
            &SCRIPT.replace("She did not answer.", "She said nothing at all."),
        );
        let kept = edited
            .manifest
            .turn_beats
            .iter()
            .find(|beat| beat.turn_id == held_turn)
            .expect("the held beat survives");
        assert_eq!(kept.lead_in_us, Microseconds(2_500_000));
        assert_eq!(kept.source, BeatSource::Explicit);
    }

    #[test]
    fn a_beat_is_dropped_with_the_line_it_belonged_to() {
        let first = apply(&project(), SCRIPT);
        let removed_turn = first.manifest.dialogue[2].id.clone();
        let with_override = hold_beat(&first.manifest, &removed_turn, 2_500_000);

        let edited = apply(
            &with_override,
            "NARRATOR: The harmattan came early.\n\nADAEZE: (quiet) You said you would come back.\n",
        );
        assert_eq!(edited.manifest.turn_beats.len(), 2);
        assert!(edited
            .manifest
            .turn_beats
            .iter()
            .all(|beat| beat.turn_id != removed_turn));
    }

    #[test]
    fn a_script_change_invalidates_speech_but_not_ingest_or_transcript() {
        let applied = apply(&project(), SCRIPT);
        assert!(applied.invalidated_stages.contains(&RevisionStage::Speech));
        assert!(!applied.invalidated_stages.contains(&RevisionStage::Ingest));
        assert!(!applied
            .invalidated_stages
            .contains(&RevisionStage::Transcript));
    }

    #[test]
    fn an_unknown_speaker_is_rejected_before_the_project_changes() {
        let error = apply_dialogue_script(
            &project(),
            &DialogueScriptRequest {
                cast: cast(),
                script: "EMEKA: Who am I?\n".into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::UnknownSpeaker);
        assert!(error.message.contains("line 1"), "{}", error.message);
    }
}
