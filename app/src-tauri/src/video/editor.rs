//! Pure, deterministic edits for the canonical Video Studio timeline.
//!
//! This module deliberately does not acquire locks, read the Store version, advance the
//! manifest revision, append revision history, or write files. The integration layer must bind
//! `base_version_id` to the current Store record, provide the project lock/idempotency boundary,
//! and commit the returned manifest with the receipt's canonical diff.

use super::contracts::{
    validate_identifier, AudioMixTrack, CaptionCue, Microseconds, ReviewedScene, RevisionStage,
    SourceAssetKind, TimeRange, TimelineClip, TimelineGap, TimelineTrack, TrackKind, Validate,
    VideoError, VideoErrorCode, VideoProjectManifest, VideoResult,
};
use super::lexicon::{fingerprint_for_character, LexiconEntry};
use super::performance::{derive_turn_beats, BeatSource, TurnBeat};
use super::score::{bed_ducking, fit_cue, MusicCue};
use super::sound::{SoundAsset, SoundLayer};
use super::timeline::{
    map_source_endpoint_to_timeline, map_timeline_endpoint_to_source, QuantizeMode,
};
use super::visuals::{VisualFit, VisualMotion};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MIN_TIMELINE_SCENE_DURATION_US: i64 = 100_000;
const MAX_TIMELINE_EDIT_OPERATIONS: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTimelineEditRequest {
    pub project_id: String,
    pub expected_revision: u64,
    /// Opaque Store version. The pure layer echoes it; the integration layer owns its CAS check.
    pub base_version_id: String,
    /// Idempotency key for the complete ordered operation batch.
    pub operation_id: String,
    pub operations: Vec<VideoTimelineOperation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoTimelineOperation {
    SplitScene {
        scene_id: String,
        at_timeline_us: Microseconds,
    },
    TrimScene {
        scene_id: String,
        source_start_us: Microseconds,
        source_end_us: Microseconds,
    },
    ReorderScene {
        scene_id: String,
        to_index: usize,
    },
    /// Exact inverse of a deterministic split. Arbitrary adjacent scenes are not mergeable.
    MergeScenes {
        first_scene_id: String,
        second_scene_id: String,
    },
    /// Overrides the silence - or overlap - before one dialogue turn. An explicit beat is the
    /// writer's decision and survives every later edit that leaves this turn's words alone.
    SetTurnBeat {
        turn_id: String,
        lead_in_us: Microseconds,
        overlap_us: Microseconds,
    },
    /// Returns one turn to the beat derived from the script.
    ClearTurnBeat {
        turn_id: String,
    },
    /// Adds or replaces one pronunciation rule. Takes produced under the rules this changes are
    /// dropped, so the affected lines are re-read and no other line is disturbed.
    SetLexiconEntry {
        entry: LexiconEntry,
    },
    /// Removes one pronunciation rule, dropping the takes it governed.
    RemoveLexiconEntry {
        entry_id: String,
    },
    /// Adds or replaces one music cue. A bed placed on a track is given its ducking envelope here
    /// rather than left for the author to remember.
    SetMusicCue {
        cue: MusicCue,
    },
    /// Removes one music cue and the mix track it owned.
    RemoveMusicCue {
        cue_id: String,
    },
    /// Places generated music on the timeline for a planned cue: fits it to the cue's target,
    /// positions it at the cue's anchor, and gives a bed its ducking envelope.
    PlaceMusicCue {
        cue_id: String,
        source_asset_id: String,
    },
    /// Registers already-imported managed media as a tagged sound-design asset. The media must
    /// have arrived through the native import path; this operation only labels it.
    RegisterSoundAsset {
        asset_id: String,
        source_asset_id: String,
        name: String,
        tags: Vec<String>,
    },
    /// Removes one sound-design asset and every placement that used it.
    RemoveSoundAsset {
        asset_id: String,
    },
    /// Marks draft lines for re-reading at final fidelity by dropping their stand-in takes.
    ///
    /// Only the named turns lose their takes, so promoting one line never re-reads the rest of the
    /// episode. That is the whole point of drafting: the expensive voice is spent only on what
    /// survived the listen.
    PromoteTurnsToFinal {
        turn_ids: Vec<String>,
    },
    /// Adds or replaces one sound-design placement. The assistant may propose a placement from a
    /// stage direction, but it only ever takes effect through this revision-checked path.
    SetSoundLayer {
        layer: SoundLayer,
    },
    /// Removes one sound-design placement.
    RemoveSoundLayer {
        layer_id: String,
    },
    /// Repositions or animates an existing managed visual without replacing its immutable bytes.
    UpdateVisualLayer {
        layer_id: String,
        scene_id: Option<String>,
        range: TimeRange,
        fit: VisualFit,
        crop: Option<super::contracts::NormalizedRect>,
        z_index: i16,
        motion: VisualMotion,
        transition_in_us: Microseconds,
        transition_out_us: Microseconds,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoTimelineChangeReceipt {
    pub project_id: String,
    pub expected_revision: u64,
    pub base_version_id: String,
    pub operation_id: String,
    pub changed_paths: Vec<String>,
    pub invalidated_stages: BTreeSet<RevisionStage>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedVideoTimelineEdit {
    pub manifest: VideoProjectManifest,
    pub receipt: VideoTimelineChangeReceipt,
}

/// Applies an ordered batch against one already-loaded manifest.
///
/// The returned manifest retains the input `revision`, `revision_history`, and `updated_at`.
/// Callers must perform the opaque Store-version check and durable idempotency/CAS commit.
pub fn apply_timeline_edit(
    manifest: &VideoProjectManifest,
    request: &VideoTimelineEditRequest,
) -> VideoResult<AppliedVideoTimelineEdit> {
    manifest.validate_strict()?;
    validate_request(manifest, request)?;
    validate_editable_scene_references(manifest)?;

    let original_revision = manifest.revision;
    let original_history = manifest.revision_history.clone();
    let original_updated_at = manifest.updated_at.clone();
    let mut edited = manifest.clone();
    for operation in &request.operations {
        match operation {
            VideoTimelineOperation::SplitScene {
                scene_id,
                at_timeline_us,
            } => split_scene(&mut edited, scene_id, *at_timeline_us)?,
            VideoTimelineOperation::TrimScene {
                scene_id,
                source_start_us,
                source_end_us,
            } => trim_scene(&mut edited, scene_id, *source_start_us, *source_end_us)?,
            VideoTimelineOperation::ReorderScene { scene_id, to_index } => {
                reorder_scene(&mut edited, scene_id, *to_index)?
            }
            VideoTimelineOperation::MergeScenes {
                first_scene_id,
                second_scene_id,
            } => merge_scenes(&mut edited, first_scene_id, second_scene_id)?,
            VideoTimelineOperation::SetTurnBeat {
                turn_id,
                lead_in_us,
                overlap_us,
            } => set_turn_beat(&mut edited, turn_id, *lead_in_us, *overlap_us)?,
            VideoTimelineOperation::ClearTurnBeat { turn_id } => {
                clear_turn_beat(&mut edited, turn_id)?
            }
            VideoTimelineOperation::SetLexiconEntry { entry } => {
                set_lexicon_entry(&mut edited, entry)?
            }
            VideoTimelineOperation::RemoveLexiconEntry { entry_id } => {
                remove_lexicon_entry(&mut edited, entry_id)?
            }
            VideoTimelineOperation::SetMusicCue { cue } => set_music_cue(&mut edited, cue)?,
            VideoTimelineOperation::RemoveMusicCue { cue_id } => {
                remove_music_cue(&mut edited, cue_id)?
            }
            VideoTimelineOperation::PlaceMusicCue {
                cue_id,
                source_asset_id,
            } => place_music_cue(&mut edited, cue_id, source_asset_id)?,
            VideoTimelineOperation::RegisterSoundAsset {
                asset_id,
                source_asset_id,
                name,
                tags,
            } => register_sound_asset(&mut edited, asset_id, source_asset_id, name, tags)?,
            VideoTimelineOperation::RemoveSoundAsset { asset_id } => {
                remove_sound_asset(&mut edited, asset_id)?
            }
            VideoTimelineOperation::PromoteTurnsToFinal { turn_ids } => {
                promote_turns_to_final(&mut edited, turn_ids)?
            }
            VideoTimelineOperation::SetSoundLayer { layer } => set_sound_layer(&mut edited, layer)?,
            VideoTimelineOperation::RemoveSoundLayer { layer_id } => {
                remove_sound_layer(&mut edited, layer_id)?
            }
            VideoTimelineOperation::UpdateVisualLayer {
                layer_id,
                scene_id,
                range,
                fit,
                crop,
                z_index,
                motion,
                transition_in_us,
                transition_out_us,
            } => update_visual_layer(
                &mut edited,
                layer_id,
                scene_id.clone(),
                *range,
                *fit,
                *crop,
                *z_index,
                motion.clone(),
                *transition_in_us,
                *transition_out_us,
            )?,
        }
        validate_editable_scene_references(&edited)?;
        edited.validate_strict()?;
    }

    if edited.revision != original_revision
        || edited.revision_history != original_history
        || edited.updated_at != original_updated_at
    {
        return Err(edit_error(
            VideoErrorCode::InvalidRevision,
            "pure timeline edits may not advance commit metadata",
            "revision",
        ));
    }

    let changed_paths = super::manifest_changed_paths(manifest, &edited).map_err(|error| {
        edit_error(
            VideoErrorCode::InvalidRevision,
            format!("canonical manifest diff failed: {error}"),
            "manifest",
        )
    })?;
    if changed_paths.is_empty() {
        return Err(edit_error(
            VideoErrorCode::InvalidRevision,
            "the requested timeline edit produced no content change",
            "operations",
        ));
    }
    let invalidated_stages = super::invalidated_stages_for_manifest_changes(&changed_paths);
    Ok(AppliedVideoTimelineEdit {
        manifest: edited,
        receipt: VideoTimelineChangeReceipt {
            project_id: request.project_id.clone(),
            expected_revision: request.expected_revision,
            base_version_id: request.base_version_id.clone(),
            operation_id: request.operation_id.clone(),
            changed_paths: changed_paths.into_iter().collect(),
            invalidated_stages,
        },
    })
}

fn validate_request(
    manifest: &VideoProjectManifest,
    request: &VideoTimelineEditRequest,
) -> VideoResult<()> {
    validate_identifier(&request.project_id, "project_id")?;
    validate_identifier(&request.base_version_id, "base_version_id")?;
    validate_identifier(&request.operation_id, "operation_id")?;
    if request.project_id != manifest.project_id {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            "timeline edit targets a different project",
            "project_id",
        ));
    }
    if request.expected_revision != manifest.revision {
        return Err(edit_error(
            VideoErrorCode::InvalidRevision,
            format!(
                "timeline edit expects revision {}, but the manifest is at {}",
                request.expected_revision, manifest.revision
            ),
            "expected_revision",
        ));
    }
    if request.operations.is_empty() || request.operations.len() > MAX_TIMELINE_EDIT_OPERATIONS {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!("timeline edit requires 1..={MAX_TIMELINE_EDIT_OPERATIONS} operations"),
            "operations",
        ));
    }
    for operation in &request.operations {
        match operation {
            VideoTimelineOperation::SplitScene { scene_id, .. }
            | VideoTimelineOperation::TrimScene { scene_id, .. }
            | VideoTimelineOperation::ReorderScene { scene_id, .. } => {
                validate_identifier(scene_id, "operations.scene_id")?
            }
            VideoTimelineOperation::MergeScenes {
                first_scene_id,
                second_scene_id,
            } => {
                validate_identifier(first_scene_id, "operations.first_scene_id")?;
                validate_identifier(second_scene_id, "operations.second_scene_id")?;
            }
            VideoTimelineOperation::SetTurnBeat { turn_id, .. }
            | VideoTimelineOperation::ClearTurnBeat { turn_id } => {
                validate_identifier(turn_id, "operations.turn_id")?
            }
            VideoTimelineOperation::SetLexiconEntry { entry } => entry.validate()?,
            VideoTimelineOperation::RemoveLexiconEntry { entry_id } => {
                validate_identifier(entry_id, "operations.entry_id")?
            }
            VideoTimelineOperation::SetMusicCue { cue } => cue.validate()?,
            VideoTimelineOperation::RemoveMusicCue { cue_id } => {
                validate_identifier(cue_id, "operations.cue_id")?
            }
            VideoTimelineOperation::PlaceMusicCue {
                cue_id,
                source_asset_id,
            } => {
                validate_identifier(cue_id, "operations.cue_id")?;
                validate_identifier(source_asset_id, "operations.source_asset_id")?;
            }
            VideoTimelineOperation::RegisterSoundAsset {
                asset_id,
                source_asset_id,
                ..
            } => {
                validate_identifier(asset_id, "operations.asset_id")?;
                validate_identifier(source_asset_id, "operations.source_asset_id")?;
            }
            VideoTimelineOperation::RemoveSoundAsset { asset_id } => {
                validate_identifier(asset_id, "operations.asset_id")?
            }
            VideoTimelineOperation::PromoteTurnsToFinal { turn_ids } => {
                if turn_ids.is_empty() {
                    return Err(edit_error(
                        VideoErrorCode::InvalidPerformance,
                        "name at least one draft line to re-read",
                        "operations.turn_ids",
                    ));
                }
                for turn_id in turn_ids {
                    validate_identifier(turn_id, "operations.turn_ids")?;
                }
            }
            VideoTimelineOperation::SetSoundLayer { layer } => layer.validate()?,
            VideoTimelineOperation::RemoveSoundLayer { layer_id } => {
                validate_identifier(layer_id, "operations.layer_id")?
            }
            VideoTimelineOperation::UpdateVisualLayer {
                layer_id, scene_id, ..
            } => {
                validate_identifier(layer_id, "operations.layer_id")?;
                if let Some(scene_id) = scene_id {
                    validate_identifier(scene_id, "operations.scene_id")?;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_visual_layer(
    manifest: &mut VideoProjectManifest,
    layer_id: &str,
    scene_id: Option<String>,
    range: TimeRange,
    fit: VisualFit,
    crop: Option<super::contracts::NormalizedRect>,
    z_index: i16,
    motion: VisualMotion,
    transition_in_us: Microseconds,
    transition_out_us: Microseconds,
) -> VideoResult<()> {
    let layer = manifest
        .visual_layers
        .iter_mut()
        .find(|layer| layer.id == layer_id)
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                format!("visual layer {layer_id} does not exist"),
                "operations.layer_id",
            )
        })?;
    layer.scene_id = scene_id;
    layer.range = range;
    layer.fit = fit;
    layer.crop = crop;
    layer.z_index = z_index;
    layer.motion = motion;
    layer.transition_in_us = transition_in_us;
    layer.transition_out_us = transition_out_us;
    layer.validate()
}

/// Hold or tighten the beat before one turn.
///
/// Marking it explicit is what makes it survive: `apply_dialogue_script` recomputes derived beats
/// from the script on every revision but carries explicit ones forward untouched.
fn set_turn_beat(
    manifest: &mut VideoProjectManifest,
    turn_id: &str,
    lead_in_us: Microseconds,
    overlap_us: Microseconds,
) -> VideoResult<()> {
    if !manifest.dialogue.iter().any(|turn| turn.id == turn_id) {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("dialogue turn {turn_id} does not exist"),
            "operations.turn_id",
        ));
    }
    let beat = TurnBeat {
        turn_id: turn_id.to_string(),
        lead_in_us,
        overlap_us,
        source: BeatSource::Explicit,
    };
    beat.validate()?;
    match manifest
        .turn_beats
        .iter_mut()
        .find(|existing| existing.turn_id == turn_id)
    {
        Some(existing) => *existing = beat,
        None => manifest.turn_beats.push(beat),
    }
    Ok(())
}

/// Return one turn to the beat the script implies.
fn clear_turn_beat(manifest: &mut VideoProjectManifest, turn_id: &str) -> VideoResult<()> {
    let Some(position) = manifest.dialogue.iter().position(|turn| turn.id == turn_id) else {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("dialogue turn {turn_id} does not exist"),
            "operations.turn_id",
        ));
    };
    let is_explicit = manifest
        .turn_beats
        .iter()
        .any(|beat| beat.turn_id == turn_id && matches!(beat.source, BeatSource::Explicit));
    if !is_explicit {
        return Err(edit_error(
            VideoErrorCode::InvalidPerformance,
            format!("dialogue turn {turn_id} already uses its derived beat"),
            "operations.turn_id",
        ));
    }
    // Re-deriving from the script is what puts this turn back in conversation with its
    // neighbours; deleting the beat alone would leave a gap the assembler cannot interpret.
    let surviving = manifest
        .turn_beats
        .iter()
        .filter(|beat| beat.turn_id != turn_id)
        .cloned()
        .collect::<Vec<_>>();
    let derived = derive_turn_beats(&manifest.dialogue, &manifest.performance_clock, &surviving)?;
    let restored = derived.get(position).cloned().ok_or_else(|| {
        edit_error(
            VideoErrorCode::InvalidPerformance,
            "the derived beat for this turn could not be recomputed",
            "operations.turn_id",
        )
    })?;
    match manifest
        .turn_beats
        .iter_mut()
        .find(|beat| beat.turn_id == turn_id)
    {
        Some(existing) => *existing = restored,
        None => manifest.turn_beats.push(restored),
    }
    Ok(())
}

fn set_lexicon_entry(manifest: &mut VideoProjectManifest, entry: &LexiconEntry) -> VideoResult<()> {
    entry.validate()?;
    match manifest
        .lexicon
        .iter_mut()
        .find(|existing| existing.id == entry.id)
    {
        Some(existing) => *existing = entry.clone(),
        None => manifest.lexicon.push(entry.clone()),
    }
    drop_takes_with_stale_pronunciation(manifest);
    Ok(())
}

fn remove_lexicon_entry(manifest: &mut VideoProjectManifest, entry_id: &str) -> VideoResult<()> {
    let before = manifest.lexicon.len();
    manifest.lexicon.retain(|entry| entry.id != entry_id);
    if manifest.lexicon.len() == before {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("pronunciation rule {entry_id} does not exist"),
            "operations.entry_id",
        ));
    }
    drop_takes_with_stale_pronunciation(manifest);
    Ok(())
}

/// Drop only the takes whose character's rules actually changed.
///
/// A rule scoped to one character cannot stale another character's lines, and a project-wide rule
/// that no line uses changes no fingerprint, so nothing is re-read for it.
fn drop_takes_with_stale_pronunciation(manifest: &mut VideoProjectManifest) {
    let character_by_turn = manifest
        .dialogue
        .iter()
        .map(|turn| (turn.id.clone(), turn.character_id.clone()))
        .collect::<BTreeMap<_, _>>();
    let fingerprint_by_character = manifest
        .cast
        .iter()
        .map(|member| {
            (
                member.id.clone(),
                fingerprint_for_character(&manifest.lexicon, &member.id),
            )
        })
        .collect::<BTreeMap<_, _>>();
    manifest.narration_bindings.retain(|binding| {
        let Some(turn_id) = binding.turn_id.as_deref() else {
            // A scene-scoped take predates the cast and is governed by no character's rules.
            return true;
        };
        let Some(character_id) = character_by_turn.get(turn_id) else {
            return true;
        };
        fingerprint_by_character
            .get(character_id)
            .cloned()
            .flatten()
            == binding.lexicon_fingerprint
    });
}

fn set_music_cue(manifest: &mut VideoProjectManifest, cue: &MusicCue) -> VideoResult<()> {
    cue.validate()?;
    match manifest.music_cues.iter_mut().find(|it| it.id == cue.id) {
        Some(existing) => *existing = cue.clone(),
        None => manifest.music_cues.push(cue.clone()),
    }
    if let Some(track_id) = cue.track_id.as_deref() {
        if cue.role.is_underscore() {
            // The sidechain is the speech track this bed plays under. Choosing it here, from the
            // takes that actually exist, is what makes the envelope real rather than a default the
            // renderer would have to resolve later.
            let speech_track_id = speech_track_id(manifest, track_id).ok_or_else(|| {
                edit_error(
                    VideoErrorCode::InvalidCue,
                    "a music bed needs narration to duck against",
                    "operations.cue",
                )
            })?;
            let ducking = bed_ducking(&speech_track_id);
            match manifest
                .audio_mix
                .tracks
                .iter_mut()
                .find(|mix| mix.track_id == track_id)
            {
                Some(mix) => {
                    mix.gain_db_milli = cue.gain_db_milli;
                    mix.ducking = Some(ducking);
                }
                None => manifest.audio_mix.tracks.push(AudioMixTrack {
                    track_id: track_id.to_string(),
                    gain_db_milli: cue.gain_db_milli,
                    pan_milli: 0,
                    ducking: Some(ducking),
                }),
            }
        } else if let Some(mix) = manifest
            .audio_mix
            .tracks
            .iter_mut()
            .find(|mix| mix.track_id == track_id)
        {
            mix.gain_db_milli = cue.gain_db_milli;
        }
    }
    Ok(())
}

fn remove_music_cue(manifest: &mut VideoProjectManifest, cue_id: &str) -> VideoResult<()> {
    let Some(index) = manifest.music_cues.iter().position(|cue| cue.id == cue_id) else {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("music cue {cue_id} does not exist"),
            "operations.cue_id",
        ));
    };
    let removed = manifest.music_cues.remove(index);
    // The mix entry existed only to carry this cue, so leaving it behind would leave a ducking
    // envelope pointing at music the project no longer has.
    if let Some(track_id) = removed.track_id.as_deref() {
        if !manifest
            .music_cues
            .iter()
            .any(|cue| cue.track_id.as_deref() == Some(track_id))
        {
            manifest
                .audio_mix
                .tracks
                .retain(|mix| mix.track_id != track_id);
        }
    }
    Ok(())
}

/// The audio track carrying narration, which is what a bed ducks against.
///
/// Preferring a track that actually holds a narration take avoids sidechaining a bed to music or
/// to another bed, which would duck against the wrong thing and sound like a fault.
fn speech_track_id(manifest: &VideoProjectManifest, exclude_track_id: &str) -> Option<String> {
    let narration_artifacts = manifest
        .narration_bindings
        .iter()
        .map(|binding| binding.render_artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    manifest
        .tracks
        .iter()
        .find(|track| {
            track.id != exclude_track_id
                && matches!(track.kind, TrackKind::Audio)
                && track.clips.iter().any(|clip| {
                    clip.media
                        .render_artifact_id
                        .as_deref()
                        .is_some_and(|id| narration_artifacts.contains(id))
                })
        })
        .map(|track| track.id.clone())
}

/// Where a cue begins on the project clock.
///
/// Resolving the anchor at placement time, from the timeline as it stands, is what lets a cue be
/// authored before the scenes and takes it refers to are final.
fn cue_timeline_start(
    manifest: &VideoProjectManifest,
    anchor: &super::score::CueAnchor,
) -> VideoResult<Microseconds> {
    use super::score::CueAnchor;
    match anchor {
        CueAnchor::Scene { scene_id } => manifest
            .reviewed_scenes
            .iter()
            .find(|scene| &scene.id == scene_id)
            .map(|scene| scene.timeline_start_us)
            .ok_or_else(|| {
                edit_error(
                    VideoErrorCode::MissingReference,
                    format!("scene {scene_id} does not exist"),
                    "operations.cue_id",
                )
            }),
        CueAnchor::Turn { turn_id } => manifest
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find(|clip| clip.turn_id.as_deref() == Some(turn_id.as_str()))
            .map(|clip| clip.timeline_start_us)
            .ok_or_else(|| {
                // A turn with no clip has not been narrated, so there is no moment to start on.
                edit_error(
                    VideoErrorCode::MissingReference,
                    format!("dialogue turn {turn_id} has no narration on the timeline yet"),
                    "operations.cue_id",
                )
            }),
        CueAnchor::AfterFinalTurn => {
            let narrated = manifest
                .narration_bindings
                .iter()
                .filter(|binding| binding.turn_id.is_some())
                .map(|binding| binding.render_artifact_id.as_str())
                .collect::<BTreeSet<_>>();
            manifest
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .filter(|clip| {
                    clip.media
                        .render_artifact_id
                        .as_deref()
                        .is_some_and(|id| narrated.contains(id))
                })
                .map(|clip| {
                    clip.timeline_start_us
                        .checked_add(clip.timeline_duration_us)
                })
                .collect::<VideoResult<Vec<_>>>()?
                .into_iter()
                .max()
                .ok_or_else(|| {
                    edit_error(
                        VideoErrorCode::MissingReference,
                        "an outro needs narration on the timeline to resolve after",
                        "operations.cue_id",
                    )
                })
        }
    }
}

fn place_music_cue(
    manifest: &mut VideoProjectManifest,
    cue_id: &str,
    source_asset_id: &str,
) -> VideoResult<()> {
    let cue = manifest
        .music_cues
        .iter()
        .find(|cue| cue.id == cue_id)
        .cloned()
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                format!("music cue {cue_id} does not exist"),
                "operations.cue_id",
            )
        })?;
    if !cue.needs_generation() {
        return Err(edit_error(
            VideoErrorCode::InvalidCue,
            format!("music cue {cue_id} already has music placed"),
            "operations.cue_id",
        ));
    }
    let source = manifest
        .source_assets
        .iter()
        .find(|asset| asset.id == source_asset_id)
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                format!("source asset {source_asset_id} does not exist"),
                "operations.source_asset_id",
            )
        })?;
    if !matches!(source.kind, SourceAssetKind::SoundArMusic) {
        return Err(edit_error(
            VideoErrorCode::InvalidCue,
            "a music cue can only be placed from registered soundAr music",
            "operations.source_asset_id",
        ));
    }

    // Fitting before placing means a cue that could not be made to land on its target is reported
    // for regeneration rather than placed at the wrong length.
    let fit = fit_cue(&cue, source.probe.duration_us)?;
    let timeline_start = cue_timeline_start(manifest, &cue.anchor)?;
    let span = fit.source_end_us.0.saturating_sub(fit.source_start_us.0);
    let track_id = format!("music-{cue_id}");
    if manifest.tracks.iter().any(|track| track.id == track_id) {
        return Err(edit_error(
            VideoErrorCode::DuplicateId,
            format!("timeline track {track_id} already exists"),
            "operations.cue_id",
        ));
    }
    let timeline_end = timeline_start.checked_add(Microseconds(span))?;
    if timeline_end > manifest.timeline_duration_us {
        return Err(edit_error(
            VideoErrorCode::InvalidCue,
            "the cue does not fit inside the project timeline at its anchor",
            "operations.cue_id",
        ));
    }

    manifest.tracks.push(TimelineTrack {
        id: track_id.clone(),
        kind: TrackKind::Audio,
        clips: vec![TimelineClip {
            id: format!("music-clip-{cue_id}"),
            scene_id: None,
            turn_id: None,
            media: super::contracts::MediaReference {
                source_asset_id: Some(source_asset_id.to_string()),
                render_artifact_id: None,
            },
            source_range: TimeRange::new(fit.source_start_us.0, fit.source_end_us.0)?,
            timeline_start_us: timeline_start,
            timeline_duration_us: Microseconds(span),
            playback_rate: super::contracts::RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        }],
        // Music occupies part of the timeline, so this track deliberately does not partition it.
        preserve_gaps: false,
    });

    let mut placed = cue;
    placed.source_asset_id = Some(source_asset_id.to_string());
    placed.track_id = Some(track_id);
    placed.fade_out_us = fit.fade_out_us;
    set_music_cue(manifest, &placed)
}

fn register_sound_asset(
    manifest: &mut VideoProjectManifest,
    asset_id: &str,
    source_asset_id: &str,
    name: &str,
    tags: &[String],
) -> VideoResult<()> {
    let source = manifest
        .source_assets
        .iter()
        .find(|source| source.id == source_asset_id)
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                format!("source asset {source_asset_id} is not registered in this project"),
                "operations.source_asset_id",
            )
        })?;
    if !source.probe.has_audio {
        return Err(edit_error(
            VideoErrorCode::InvalidAsset,
            "sound design needs media that actually contains audio",
            "operations.source_asset_id",
        ));
    }
    // One managed source is one sound. Registering it twice would let two labels drift apart while
    // describing identical audio.
    if manifest
        .sound_assets
        .iter()
        .any(|asset| asset.source_asset_id == source_asset_id && asset.id != asset_id)
    {
        return Err(edit_error(
            VideoErrorCode::DuplicateId,
            "that managed source is already registered as a sound asset",
            "operations.source_asset_id",
        ));
    }
    let asset = SoundAsset {
        id: asset_id.to_string(),
        name: name.to_string(),
        source_asset_id: source_asset_id.to_string(),
        tags: tags.to_vec(),
        created_at: source.provenance.imported_at.clone(),
    };
    asset.validate()?;
    match manifest
        .sound_assets
        .iter_mut()
        .find(|existing| existing.id == asset_id)
    {
        Some(existing) => *existing = asset,
        None => manifest.sound_assets.push(asset),
    }
    Ok(())
}

fn remove_sound_asset(manifest: &mut VideoProjectManifest, asset_id: &str) -> VideoResult<()> {
    let before = manifest.sound_assets.len();
    manifest.sound_assets.retain(|asset| asset.id != asset_id);
    if manifest.sound_assets.len() == before {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("sound asset {asset_id} does not exist"),
            "operations.asset_id",
        ));
    }
    // A placement without its asset cannot be rendered, so removing the sound removes its uses
    // rather than leaving the manifest unvalidatable.
    manifest
        .sound_layers
        .retain(|layer| layer.asset_id != asset_id);
    Ok(())
}

fn promote_turns_to_final(
    manifest: &mut VideoProjectManifest,
    turn_ids: &[String],
) -> VideoResult<()> {
    let wanted = turn_ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let draft_turns = manifest
        .draft_turn_ids()
        .into_iter()
        .collect::<BTreeSet<_>>();
    // Naming a line that is already final would read as a promotion that did something.
    if let Some(missing) = wanted.iter().find(|id| !draft_turns.contains(*id)) {
        return Err(edit_error(
            VideoErrorCode::InvalidPerformance,
            format!("dialogue turn {missing} does not have a draft take to promote"),
            "operations.turn_ids",
        ));
    }
    manifest.narration_bindings.retain(|binding| {
        !binding
            .turn_id
            .as_deref()
            .is_some_and(|turn_id| wanted.contains(turn_id))
    });
    Ok(())
}

fn set_sound_layer(manifest: &mut VideoProjectManifest, layer: &SoundLayer) -> VideoResult<()> {
    layer.validate()?;
    // Placements reference registered assets only. Accepting an unknown id here would let a
    // proposal from a stage direction invent a sound the project does not have.
    if !manifest
        .sound_assets
        .iter()
        .any(|asset| asset.id == layer.asset_id)
    {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("sound asset {} is not registered", layer.asset_id),
            "operations.layer.asset_id",
        ));
    }
    match manifest
        .sound_layers
        .iter_mut()
        .find(|existing| existing.id == layer.id)
    {
        Some(existing) => *existing = layer.clone(),
        None => manifest.sound_layers.push(layer.clone()),
    }
    Ok(())
}

fn remove_sound_layer(manifest: &mut VideoProjectManifest, layer_id: &str) -> VideoResult<()> {
    let before = manifest.sound_layers.len();
    manifest.sound_layers.retain(|layer| layer.id != layer_id);
    if manifest.sound_layers.len() == before {
        return Err(edit_error(
            VideoErrorCode::MissingReference,
            format!("sound placement {layer_id} does not exist"),
            "operations.layer_id",
        ));
    }
    Ok(())
}

fn validate_editable_scene_references(manifest: &VideoProjectManifest) -> VideoResult<()> {
    let mut previous_end = Microseconds::ZERO;
    let scene_ranges = manifest
        .reviewed_scenes
        .iter()
        .map(|scene| Ok((scene.id.as_str(), scene_timeline_range(scene)?)))
        .collect::<VideoResult<BTreeMap<_, _>>>()?;
    for scene in &manifest.reviewed_scenes {
        let range = scene_ranges[scene.id.as_str()];
        if range.start_us < previous_end {
            return Err(edit_error(
                VideoErrorCode::TimelineOverlap,
                "reviewed scenes must be stored in non-overlapping timeline order",
                "reviewed_scenes",
            ));
        }
        previous_end = range.end_us;
    }
    for clip in manifest.tracks.iter().flat_map(|track| &track.clips) {
        let Some(scene_id) = clip.scene_id.as_deref() else {
            continue;
        };
        let scene_range = scene_ranges.get(scene_id).ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                "timeline clip references a missing reviewed scene",
                "tracks.clips.scene_id",
            )
        })?;
        if !range_contains(*scene_range, clip.timeline_range()?) {
            return Err(edit_error(
                VideoErrorCode::InvalidTrack,
                "scene-owned clips must remain inside their reviewed scene",
                "tracks.clips.timeline",
            ));
        }
    }
    for caption in &manifest.captions {
        let Some(scene_id) = caption.scene_id.as_deref() else {
            continue;
        };
        let scene_range = scene_ranges.get(scene_id).ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                "caption references a missing reviewed scene",
                "captions.scene_id",
            )
        })?;
        if !range_contains(*scene_range, caption.range) {
            return Err(edit_error(
                VideoErrorCode::InvalidCaption,
                "scene-owned captions must remain inside their reviewed scene",
                "captions.range",
            ));
        }
    }
    for layer in &manifest.visual_layers {
        let Some(scene_id) = layer.scene_id.as_deref() else {
            continue;
        };
        let scene_range = scene_ranges.get(scene_id).ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                "visual layer references a missing reviewed scene",
                "visual_layers.scene_id",
            )
        })?;
        if !range_contains(*scene_range, layer.range) {
            return Err(edit_error(
                VideoErrorCode::InvalidLayout,
                "scene-owned visual layers must remain inside their reviewed scene",
                "visual_layers.range",
            ));
        }
    }
    Ok(())
}

fn scene_timeline_range(scene: &ReviewedScene) -> VideoResult<TimeRange> {
    let end = scene
        .timeline_start_us
        .checked_add(scene.timeline_duration_us)?;
    TimeRange::new(scene.timeline_start_us.0, end.0)
}

fn range_contains(outer: TimeRange, inner: TimeRange) -> bool {
    inner.start_us >= outer.start_us && inner.end_us <= outer.end_us
}

fn find_scene_index(manifest: &VideoProjectManifest, scene_id: &str) -> VideoResult<usize> {
    manifest
        .reviewed_scenes
        .iter()
        .position(|scene| scene.id == scene_id)
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::MissingReference,
                format!("reviewed scene {scene_id} does not exist"),
                "operations.scene_id",
            )
        })
}

fn find_source_anchor(
    manifest: &VideoProjectManifest,
    scene: &ReviewedScene,
) -> VideoResult<TimelineClip> {
    let source_asset_id = scene.source_asset_id.as_deref().ok_or_else(|| {
        edit_error(
            VideoErrorCode::InvalidScene,
            "split and trim require a source-backed scene",
            "reviewed_scenes.source_asset_id",
        )
    })?;
    let source_range = scene.source_range.ok_or_else(|| {
        edit_error(
            VideoErrorCode::InvalidScene,
            "split and trim require a source-clock scene range",
            "reviewed_scenes.source_range",
        )
    })?;
    let timeline_range = scene_timeline_range(scene)?;
    manifest
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .filter(|clip| {
            clip.scene_id.as_deref() == Some(scene.id.as_str())
                && clip.media.source_asset_id.as_deref() == Some(source_asset_id)
                && clip.source_range == source_range
                && clip.timeline_range().ok() == Some(timeline_range)
        })
        .min_by(|left, right| left.id.cmp(&right.id))
        .cloned()
        .ok_or_else(|| {
            edit_error(
                VideoErrorCode::InvalidTrack,
                "source-backed scene requires a full-span canonical source clip",
                "tracks.clips",
            )
        })
}

fn map_timeline_endpoint_to_source_exact(
    clip: &TimelineClip,
    timeline_us: Microseconds,
) -> VideoResult<Microseconds> {
    let source = map_timeline_endpoint_to_source(clip, timeline_us, QuantizeMode::Floor)?;
    let round_trip = map_source_endpoint_to_timeline(clip, source, QuantizeMode::Floor)?;
    if round_trip != timeline_us {
        return Err(edit_error(
            VideoErrorCode::DurationMismatch,
            "timeline boundary does not map to an exact source microsecond",
            "operations.at_timeline_us",
        ));
    }
    Ok(source)
}

fn map_source_endpoint_to_timeline_exact(
    clip: &TimelineClip,
    source_us: Microseconds,
) -> VideoResult<Microseconds> {
    let timeline = map_source_endpoint_to_timeline(clip, source_us, QuantizeMode::Floor)?;
    let round_trip = map_timeline_endpoint_to_source(clip, timeline, QuantizeMode::Floor)?;
    if round_trip != source_us {
        return Err(edit_error(
            VideoErrorCode::DurationMismatch,
            "source boundary does not map to an exact timeline microsecond",
            "operations.source_start_us",
        ));
    }
    Ok(timeline)
}

fn arithmetic_overflow() -> VideoError {
    VideoError::new(
        VideoErrorCode::ArithmeticOverflow,
        "timeline edit arithmetic overflowed",
    )
}

fn edit_error(
    code: VideoErrorCode,
    message: impl Into<String>,
    field: impl Into<String>,
) -> VideoError {
    VideoError::new(code, message).at(field)
}

fn split_scene(
    manifest: &mut VideoProjectManifest,
    scene_id: &str,
    split_timeline_us: Microseconds,
) -> VideoResult<()> {
    let scene_index = find_scene_index(manifest, scene_id)?;
    let original = manifest.reviewed_scenes[scene_index].clone();
    let timeline_range = scene_timeline_range(&original)?;
    let left_duration = split_timeline_us
        .0
        .checked_sub(timeline_range.start_us.0)
        .ok_or_else(arithmetic_overflow)?;
    let right_duration = timeline_range
        .end_us
        .0
        .checked_sub(split_timeline_us.0)
        .ok_or_else(arithmetic_overflow)?;
    if left_duration < MIN_TIMELINE_SCENE_DURATION_US
        || right_duration < MIN_TIMELINE_SCENE_DURATION_US
    {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!(
                "both split scenes must be at least {MIN_TIMELINE_SCENE_DURATION_US} microseconds"
            ),
            "operations.at_timeline_us",
        ));
    }

    let anchor = find_source_anchor(manifest, &original)?;
    if manifest
        .narration_bindings
        .iter()
        .any(|binding| binding.scene_id.as_deref() == Some(scene_id))
        || manifest
            .render_artifacts
            .iter()
            .any(|artifact| artifact.scene_id.as_deref() == Some(scene_id))
    {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            "rendered or narrated scenes must be regenerated before they can be split safely",
            "reviewed_scenes",
        ));
    }
    for layer in &manifest.visual_layers {
        ensure_span_does_not_cross_scene_boundary(
            layer.range,
            timeline_range,
            VideoErrorCode::InvalidLayout,
            "visual_layers.range",
        )?;
        if range_contains(timeline_range, layer.range)
            && layer.range.start_us < split_timeline_us
            && layer.range.end_us > split_timeline_us
        {
            return Err(edit_error(
                VideoErrorCode::InvalidLayout,
                "split the visual layer at the playhead or move it wholly to one side before splitting this scene",
                "visual_layers.range",
            ));
        }
    }
    let split_source_us = map_timeline_endpoint_to_source_exact(&anchor, split_timeline_us)?;
    let original_source = original.source_range.ok_or_else(|| {
        edit_error(
            VideoErrorCode::InvalidScene,
            "a source-mapped scene is required for this split",
            "reviewed_scenes.source_range",
        )
    })?;
    let left_source_duration = split_source_us
        .0
        .checked_sub(original_source.start_us.0)
        .ok_or_else(arithmetic_overflow)?;
    let right_source_duration = original_source
        .end_us
        .0
        .checked_sub(split_source_us.0)
        .ok_or_else(arithmetic_overflow)?;
    if left_source_duration < MIN_TIMELINE_SCENE_DURATION_US
        || right_source_duration < MIN_TIMELINE_SCENE_DURATION_US
    {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!(
                "both split source ranges must be at least {MIN_TIMELINE_SCENE_DURATION_US} microseconds"
            ),
            "operations.at_timeline_us",
        ));
    }

    let split_revision = original.revision.checked_add(1).ok_or_else(|| {
        edit_error(
            VideoErrorCode::ArithmeticOverflow,
            "scene revision overflowed during split",
            "reviewed_scenes.revision",
        )
    })?;
    let right_scene_id = deterministic_split_id("scene", &original.id, split_timeline_us);
    if manifest
        .reviewed_scenes
        .iter()
        .any(|scene| scene.id == right_scene_id)
    {
        return Err(edit_error(
            VideoErrorCode::DuplicateId,
            "deterministic split scene identifier already exists",
            "reviewed_scenes.id",
        ));
    }

    let mut left = original.clone();
    left.source_range = Some(TimeRange::new(
        original_source.start_us.0,
        split_source_us.0,
    )?);
    left.timeline_duration_us = Microseconds(left_duration);
    left.revision = split_revision;
    let mut right = original.clone();
    right.id = right_scene_id.clone();
    right.source_range = Some(TimeRange::new(split_source_us.0, original_source.end_us.0)?);
    right.timeline_start_us = split_timeline_us;
    right.timeline_duration_us = Microseconds(right_duration);
    right.revision = split_revision;
    manifest
        .reviewed_scenes
        .splice(scene_index..=scene_index, [left, right]);

    let mut layout_ids = manifest
        .layout
        .elements
        .iter()
        .map(|element| element.id.clone())
        .collect::<BTreeSet<_>>();
    let mut layout_elements = Vec::with_capacity(manifest.layout.elements.len().saturating_add(1));
    for element in &manifest.layout.elements {
        layout_elements.push(element.clone());
        if element.scene_id.as_deref() == Some(scene_id) {
            let mut right_element = element.clone();
            right_element.id = deterministic_split_id("layout", &element.id, split_timeline_us);
            if !layout_ids.insert(right_element.id.clone()) {
                return Err(edit_error(
                    VideoErrorCode::DuplicateId,
                    "deterministic split layout identifier already exists",
                    "layout.elements.id",
                ));
            }
            right_element.scene_id = Some(right_scene_id.clone());
            layout_elements.push(right_element);
        }
    }
    manifest.layout.elements = layout_elements;

    let mut clip_ids = manifest
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(|clip| clip.id.clone()))
        .collect::<BTreeSet<_>>();
    for track in &mut manifest.tracks {
        let mut edited = Vec::with_capacity(track.clips.len().saturating_add(1));
        for mut clip in track.clips.drain(..) {
            let clip_range = clip.timeline_range()?;
            ensure_span_does_not_cross_scene_boundary(
                clip_range,
                timeline_range,
                VideoErrorCode::InvalidTrack,
                "tracks.clips.timeline",
            )?;
            if !range_contains(timeline_range, clip_range) {
                edited.push(clip);
                continue;
            }
            if clip_range.end_us <= split_timeline_us {
                edited.push(clip);
                continue;
            }
            if clip_range.start_us >= split_timeline_us {
                if clip.scene_id.as_deref() == Some(scene_id) {
                    clip.scene_id = Some(right_scene_id.clone());
                }
                edited.push(clip);
                continue;
            }

            let source_split = map_timeline_endpoint_to_source_exact(&clip, split_timeline_us)?;
            let original_clip = clip.clone();
            clip.source_range =
                TimeRange::new(original_clip.source_range.start_us.0, source_split.0)?;
            clip.timeline_duration_us = Microseconds(
                split_timeline_us
                    .0
                    .checked_sub(original_clip.timeline_start_us.0)
                    .ok_or_else(arithmetic_overflow)?,
            );
            let mut right_clip = original_clip.clone();
            right_clip.id = deterministic_split_id("clip", &original_clip.id, split_timeline_us);
            if !clip_ids.insert(right_clip.id.clone()) {
                return Err(edit_error(
                    VideoErrorCode::DuplicateId,
                    "deterministic split clip identifier already exists",
                    "tracks.clips.id",
                ));
            }
            right_clip.source_range =
                TimeRange::new(source_split.0, original_clip.source_range.end_us.0)?;
            right_clip.timeline_start_us = split_timeline_us;
            right_clip.timeline_duration_us = Microseconds(
                original_clip
                    .timeline_start_us
                    .checked_add(original_clip.timeline_duration_us)?
                    .0
                    .checked_sub(split_timeline_us.0)
                    .ok_or_else(arithmetic_overflow)?,
            );
            if original_clip.scene_id.as_deref() == Some(scene_id) {
                clip.scene_id = Some(scene_id.to_string());
                right_clip.scene_id = Some(right_scene_id.clone());
            }
            edited.push(clip);
            edited.push(right_clip);
        }
        track.clips = edited;
    }

    let mut gap_ids = manifest
        .gaps
        .iter()
        .map(|gap| gap.id.clone())
        .collect::<BTreeSet<_>>();
    let mut gaps = Vec::with_capacity(manifest.gaps.len().saturating_add(1));
    for mut gap in manifest.gaps.drain(..) {
        ensure_span_does_not_cross_scene_boundary(
            gap.range,
            timeline_range,
            VideoErrorCode::InvalidGap,
            "gaps.range",
        )?;
        if !range_contains(timeline_range, gap.range)
            || gap.range.end_us <= split_timeline_us
            || gap.range.start_us >= split_timeline_us
        {
            gaps.push(gap);
            continue;
        }
        let original_gap = gap.clone();
        gap.range = TimeRange::new(original_gap.range.start_us.0, split_timeline_us.0)?;
        let mut right_gap = original_gap.clone();
        right_gap.id = deterministic_split_id("gap", &original_gap.id, split_timeline_us);
        if !gap_ids.insert(right_gap.id.clone()) {
            return Err(edit_error(
                VideoErrorCode::DuplicateId,
                "deterministic split gap identifier already exists",
                "gaps.id",
            ));
        }
        right_gap.range = TimeRange::new(split_timeline_us.0, original_gap.range.end_us.0)?;
        if let Some(source_range) = original_gap.source_range {
            let source_split = source_range.start_us.checked_add(Microseconds(
                split_timeline_us
                    .0
                    .checked_sub(original_gap.range.start_us.0)
                    .ok_or_else(arithmetic_overflow)?,
            ))?;
            gap.source_range = Some(TimeRange::new(source_range.start_us.0, source_split.0)?);
            right_gap.source_range = Some(TimeRange::new(source_split.0, source_range.end_us.0)?);
        }
        gaps.push(gap);
        gaps.push(right_gap);
    }
    manifest.gaps = gaps;

    let mut caption_ids = manifest
        .captions
        .iter()
        .map(|caption| caption.id.clone())
        .collect::<BTreeSet<_>>();
    let transcript = manifest.transcript.clone();
    let mut captions = Vec::with_capacity(manifest.captions.len().saturating_add(1));
    for mut caption in manifest.captions.drain(..) {
        ensure_span_does_not_cross_scene_boundary(
            caption.range,
            timeline_range,
            VideoErrorCode::InvalidCaption,
            "captions.range",
        )?;
        if !range_contains(timeline_range, caption.range) {
            captions.push(caption);
            continue;
        }
        if caption.range.end_us <= split_timeline_us {
            captions.push(caption);
            continue;
        }
        if caption.range.start_us >= split_timeline_us {
            if caption.scene_id.as_deref() == Some(scene_id) {
                caption.scene_id = Some(right_scene_id.clone());
            }
            captions.push(caption);
            continue;
        }

        let original_caption = caption.clone();
        let split_index = caption_split_byte_index_for_transcript(
            transcript.as_ref(),
            &original_caption,
            split_timeline_us,
            split_source_us,
            true,
        )?;
        caption.range = TimeRange::new(original_caption.range.start_us.0, split_timeline_us.0)?;
        caption.text = original_caption.text[..split_index].to_string();
        let mut right_caption = original_caption.clone();
        right_caption.id =
            deterministic_split_id("caption", &original_caption.id, split_timeline_us);
        if !caption_ids.insert(right_caption.id.clone()) {
            return Err(edit_error(
                VideoErrorCode::DuplicateId,
                "deterministic split caption identifier already exists",
                "captions.id",
            ));
        }
        right_caption.range = TimeRange::new(split_timeline_us.0, original_caption.range.end_us.0)?;
        right_caption.text = original_caption.text[split_index..].to_string();
        if original_caption.scene_id.as_deref() == Some(scene_id) {
            caption.scene_id = Some(scene_id.to_string());
            right_caption.scene_id = Some(right_scene_id.clone());
        }
        caption.validate()?;
        right_caption.validate()?;
        captions.push(caption);
        captions.push(right_caption);
    }
    manifest.captions = captions;
    for layer in &mut manifest.visual_layers {
        if layer.scene_id.as_deref() == Some(scene_id) && layer.range.start_us >= split_timeline_us
        {
            layer.scene_id = Some(right_scene_id.clone());
        }
    }
    Ok(())
}

fn ensure_span_does_not_cross_scene_boundary(
    span: TimeRange,
    scene: TimeRange,
    code: VideoErrorCode,
    field: &str,
) -> VideoResult<()> {
    let disjoint = span.end_us <= scene.start_us || span.start_us >= scene.end_us;
    if disjoint || range_contains(scene, span) {
        return Ok(());
    }
    Err(edit_error(
        code,
        "timeline item crosses an edited scene boundary and cannot be transformed safely",
        field,
    ))
}

fn deterministic_split_id(kind: &str, original_id: &str, split_us: Microseconds) -> String {
    let mut digest = Sha256::new();
    digest.update(b"soundar-video-timeline-split-v1\0");
    digest.update(kind.as_bytes());
    digest.update(b"\0");
    digest.update(original_id.as_bytes());
    digest.update(b"\0");
    digest.update(split_us.0.to_be_bytes());
    let hash = format!("{:x}", digest.finalize());
    format!("{kind}-split-{}", &hash[..24])
}

fn token_start_byte_indices(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut in_token = false;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            in_token = false;
        } else if !in_token {
            starts.push(index);
            in_token = true;
        }
    }
    starts
}

fn transcript_word_byte_index_from(
    transcript: &super::contracts::TranscriptVersion,
    caption: &CaptionCue,
    split_source_us: Microseconds,
    token_starts: &[usize],
) -> Option<usize> {
    let segment_id = caption.transcript_segment_id.as_deref()?;
    let segment = transcript
        .segments
        .iter()
        .find(|segment| segment.id == segment_id)?;
    if segment.word_ids.len() != token_starts.len() {
        return None;
    }
    let words = transcript
        .words
        .iter()
        .map(|word| (word.id.as_str(), word))
        .collect::<BTreeMap<_, _>>();
    let mut left_count = 0;
    for word_id in &segment.word_ids {
        let word = words.get(word_id.as_str())?;
        let midpoint = (i128::from(word.range.start_us.0) + i128::from(word.range.end_us.0)) / 2;
        if midpoint <= i128::from(split_source_us.0) {
            left_count += 1;
        }
    }
    if left_count == 0 {
        Some(0)
    } else if left_count == token_starts.len() {
        Some(caption.text.len())
    } else {
        Some(token_starts[left_count])
    }
}

fn proportional_index(offset: i64, duration: i64, item_count: usize) -> VideoResult<usize> {
    if offset <= 0 || duration <= 0 || offset >= duration || item_count < 2 {
        return Err(arithmetic_overflow());
    }
    let scaled = i128::from(offset)
        .checked_mul(i128::try_from(item_count).map_err(|_| arithmetic_overflow())?)
        .ok_or_else(arithmetic_overflow)?;
    let rounded = scaled
        .checked_add(i128::from(duration / 2))
        .ok_or_else(arithmetic_overflow)?
        / i128::from(duration);
    let index = usize::try_from(rounded).map_err(|_| arithmetic_overflow())?;
    Ok(index.clamp(1, item_count - 1))
}

fn nonempty_text_halves(text: &str, index: usize) -> bool {
    !text[..index].trim().is_empty() && !text[index..].trim().is_empty()
}

fn trim_scene(
    manifest: &mut VideoProjectManifest,
    scene_id: &str,
    source_start_us: Microseconds,
    source_end_us: Microseconds,
) -> VideoResult<()> {
    let scene_index = find_scene_index(manifest, scene_id)?;
    let original = manifest.reviewed_scenes[scene_index].clone();
    let original_source = original.source_range.ok_or_else(|| {
        edit_error(
            VideoErrorCode::InvalidScene,
            "trim requires a source-clock scene range",
            "reviewed_scenes.source_range",
        )
    })?;
    if source_start_us < original_source.start_us
        || source_end_us > original_source.end_us
        || source_end_us <= source_start_us
    {
        return Err(edit_error(
            VideoErrorCode::InvalidTimestamp,
            "trim source range must be a non-empty subset of the current scene source range",
            "operations.source_start_us",
        ));
    }
    if source_end_us.0 - source_start_us.0 < MIN_TIMELINE_SCENE_DURATION_US {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!(
                "trimmed source range must be at least {MIN_TIMELINE_SCENE_DURATION_US} microseconds"
            ),
            "operations.source_end_us",
        ));
    }
    if source_start_us == original_source.start_us && source_end_us == original_source.end_us {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            "trim operation must change at least one scene boundary",
            "operations",
        ));
    }

    let anchor = find_source_anchor(manifest, &original)?;
    let retained_timeline_start = map_source_endpoint_to_timeline_exact(&anchor, source_start_us)?;
    let retained_timeline_end = map_source_endpoint_to_timeline_exact(&anchor, source_end_us)?;
    let retained_timeline_duration = retained_timeline_end
        .0
        .checked_sub(retained_timeline_start.0)
        .ok_or_else(arithmetic_overflow)?;
    if retained_timeline_duration < MIN_TIMELINE_SCENE_DURATION_US {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!(
                "trimmed timeline scene must be at least {MIN_TIMELINE_SCENE_DURATION_US} microseconds"
            ),
            "operations.source_end_us",
        ));
    }
    let original_timeline = scene_timeline_range(&original)?;
    let retained_timeline = TimeRange::new(retained_timeline_start.0, retained_timeline_end.0)?;
    let removed_duration = original
        .timeline_duration_us
        .0
        .checked_sub(retained_timeline_duration)
        .ok_or_else(arithmetic_overflow)?;
    let new_timeline_duration = manifest
        .timeline_duration_us
        .0
        .checked_sub(removed_duration)
        .ok_or_else(arithmetic_overflow)?;
    if new_timeline_duration <= 0 {
        return Err(edit_error(
            VideoErrorCode::InvalidTimestamp,
            "trim would remove the complete project timeline",
            "timeline_duration_us",
        ));
    }
    let target_revision = original.revision.checked_add(1).ok_or_else(|| {
        edit_error(
            VideoErrorCode::ArithmeticOverflow,
            "scene revision overflowed during trim",
            "reviewed_scenes.revision",
        )
    })?;
    for layer in &manifest.visual_layers {
        ensure_span_does_not_cross_scene_boundary(
            layer.range,
            original_timeline,
            VideoErrorCode::InvalidLayout,
            "visual_layers.range",
        )?;
        if range_contains(original_timeline, layer.range)
            && !range_contains(retained_timeline, layer.range)
        {
            return Err(edit_error(
                VideoErrorCode::InvalidLayout,
                "move or resize visual layers into the retained scene range before trimming",
                "visual_layers.range",
            ));
        }
    }

    for (index, scene) in manifest.reviewed_scenes.iter_mut().enumerate() {
        if index == scene_index {
            scene.source_range = Some(TimeRange::new(source_start_us.0, source_end_us.0)?);
            scene.timeline_duration_us = Microseconds(retained_timeline_duration);
            scene.revision = target_revision;
        } else if scene.timeline_start_us >= original_timeline.end_us {
            scene.timeline_start_us = checked_shift(scene.timeline_start_us, -removed_duration)?;
        }
    }

    for track in &mut manifest.tracks {
        let mut clips = Vec::with_capacity(track.clips.len());
        for clip in track.clips.drain(..) {
            let clip_range = clip.timeline_range()?;
            ensure_span_does_not_cross_scene_boundary(
                clip_range,
                original_timeline,
                VideoErrorCode::InvalidTrack,
                "tracks.clips.timeline",
            )?;
            if range_contains(original_timeline, clip_range) {
                if let Some(trimmed) =
                    trim_clip_to_timeline(clip, retained_timeline, original_timeline.start_us)?
                {
                    clips.push(trimmed);
                }
            } else {
                let mut clip = clip;
                if clip.timeline_start_us >= original_timeline.end_us {
                    clip.timeline_start_us =
                        checked_shift(clip.timeline_start_us, -removed_duration)?;
                }
                clips.push(clip);
            }
        }
        track.clips = clips;
    }

    let mut gaps = Vec::with_capacity(manifest.gaps.len());
    for gap in manifest.gaps.drain(..) {
        ensure_span_does_not_cross_scene_boundary(
            gap.range,
            original_timeline,
            VideoErrorCode::InvalidGap,
            "gaps.range",
        )?;
        if range_contains(original_timeline, gap.range) {
            if let Some(trimmed) =
                trim_gap_to_timeline(gap, retained_timeline, original_timeline.start_us)?
            {
                gaps.push(trimmed);
            }
        } else {
            let mut gap = gap;
            if gap.range.start_us >= original_timeline.end_us {
                gap.range = checked_shift_range(gap.range, -removed_duration)?;
            }
            gaps.push(gap);
        }
    }
    manifest.gaps = gaps;

    let transcript = manifest.transcript.clone();
    let mut captions = Vec::with_capacity(manifest.captions.len());
    for caption in manifest.captions.drain(..) {
        ensure_span_does_not_cross_scene_boundary(
            caption.range,
            original_timeline,
            VideoErrorCode::InvalidCaption,
            "captions.range",
        )?;
        if range_contains(original_timeline, caption.range) {
            if let Some(trimmed) = trim_caption_to_timeline(
                transcript.as_ref(),
                &anchor,
                caption,
                retained_timeline,
                original_timeline.start_us,
            )? {
                captions.push(trimmed);
            }
        } else {
            let mut caption = caption;
            if caption.range.start_us >= original_timeline.end_us {
                caption.range = checked_shift_range(caption.range, -removed_duration)?;
            }
            captions.push(caption);
        }
    }
    manifest.captions = captions;
    for layer in &mut manifest.visual_layers {
        if range_contains(original_timeline, layer.range) {
            layer.range = shifted_intersection(
                layer.range,
                retained_timeline.start_us,
                original_timeline.start_us,
            )?;
        } else if layer.range.start_us >= original_timeline.end_us {
            layer.range = checked_shift_range(layer.range, -removed_duration)?;
        }
    }
    manifest.visual_layers.sort_by(|left, right| {
        (left.range.start_us, left.z_index, &left.id).cmp(&(
            right.range.start_us,
            right.z_index,
            &right.id,
        ))
    });
    manifest.timeline_duration_us = Microseconds(new_timeline_duration);
    Ok(())
}

fn trim_clip_to_timeline(
    mut clip: TimelineClip,
    retained: TimeRange,
    new_scene_start: Microseconds,
) -> VideoResult<Option<TimelineClip>> {
    let original_range = clip.timeline_range()?;
    let Some(intersection) = range_intersection(original_range, retained)? else {
        return Ok(None);
    };
    let source_start = map_timeline_endpoint_to_source_exact(&clip, intersection.start_us)?;
    let source_end = map_timeline_endpoint_to_source_exact(&clip, intersection.end_us)?;
    clip.source_range = TimeRange::new(source_start.0, source_end.0)?;
    clip.timeline_start_us = new_scene_start.checked_add(Microseconds(
        intersection
            .start_us
            .0
            .checked_sub(retained.start_us.0)
            .ok_or_else(arithmetic_overflow)?,
    ))?;
    clip.timeline_duration_us = intersection.duration()?;
    Ok(Some(clip))
}

fn trim_gap_to_timeline(
    mut gap: TimelineGap,
    retained: TimeRange,
    new_scene_start: Microseconds,
) -> VideoResult<Option<TimelineGap>> {
    let original_range = gap.range;
    let Some(intersection) = range_intersection(original_range, retained)? else {
        return Ok(None);
    };
    gap.range = shifted_intersection(intersection, retained.start_us, new_scene_start)?;
    if let Some(source_range) = gap.source_range {
        let source_start = source_range.start_us.checked_add(Microseconds(
            intersection
                .start_us
                .0
                .checked_sub(original_range.start_us.0)
                .ok_or_else(arithmetic_overflow)?,
        ))?;
        let source_end = source_start.checked_add(intersection.duration()?)?;
        gap.source_range = Some(TimeRange::new(source_start.0, source_end.0)?);
    }
    Ok(Some(gap))
}

fn trim_caption_to_timeline(
    transcript: Option<&super::contracts::TranscriptVersion>,
    anchor: &TimelineClip,
    mut caption: CaptionCue,
    retained: TimeRange,
    new_scene_start: Microseconds,
) -> VideoResult<Option<CaptionCue>> {
    let original_range = caption.range;
    let Some(intersection) = range_intersection(original_range, retained)? else {
        return Ok(None);
    };
    let start_index = if intersection.start_us > original_range.start_us {
        let source = map_timeline_endpoint_to_source_exact(anchor, intersection.start_us)?;
        caption_split_byte_index_for_transcript(
            transcript,
            &caption,
            intersection.start_us,
            source,
            false,
        )?
    } else {
        0
    };
    let end_index = if intersection.end_us < original_range.end_us {
        let source = map_timeline_endpoint_to_source_exact(anchor, intersection.end_us)?;
        caption_split_byte_index_for_transcript(
            transcript,
            &caption,
            intersection.end_us,
            source,
            false,
        )?
    } else {
        caption.text.len()
    };
    if start_index >= end_index || caption.text[start_index..end_index].trim().is_empty() {
        return Ok(None);
    }
    caption.text = caption.text[start_index..end_index].to_string();
    caption.range = shifted_intersection(intersection, retained.start_us, new_scene_start)?;
    caption.validate()?;
    Ok(Some(caption))
}

fn shifted_intersection(
    intersection: TimeRange,
    old_origin: Microseconds,
    new_origin: Microseconds,
) -> VideoResult<TimeRange> {
    let start_offset = intersection
        .start_us
        .0
        .checked_sub(old_origin.0)
        .ok_or_else(arithmetic_overflow)?;
    let start = new_origin.checked_add(Microseconds(start_offset))?;
    let end = start.checked_add(intersection.duration()?)?;
    TimeRange::new(start.0, end.0)
}

fn range_intersection(left: TimeRange, right: TimeRange) -> VideoResult<Option<TimeRange>> {
    let start = left.start_us.max(right.start_us);
    let end = left.end_us.min(right.end_us);
    if start >= end {
        Ok(None)
    } else {
        TimeRange::new(start.0, end.0).map(Some)
    }
}

fn checked_shift(value: Microseconds, delta: i64) -> VideoResult<Microseconds> {
    value
        .0
        .checked_add(delta)
        .map(Microseconds)
        .ok_or_else(arithmetic_overflow)
}

fn checked_shift_range(range: TimeRange, delta: i64) -> VideoResult<TimeRange> {
    TimeRange::new(
        checked_shift(range.start_us, delta)?.0,
        checked_shift(range.end_us, delta)?.0,
    )
}

fn caption_split_byte_index_for_transcript(
    transcript: Option<&super::contracts::TranscriptVersion>,
    caption: &CaptionCue,
    split_timeline_us: Microseconds,
    split_source_us: Microseconds,
    require_nonempty_halves: bool,
) -> VideoResult<usize> {
    if split_timeline_us <= caption.range.start_us || split_timeline_us >= caption.range.end_us {
        return Err(edit_error(
            VideoErrorCode::InvalidCaption,
            "caption text may only be partitioned at an interior cue boundary",
            "captions.range",
        ));
    }
    let token_starts = token_start_byte_indices(&caption.text);
    if token_starts.len() >= 2 {
        if let Some(byte_index) = transcript.and_then(|transcript| {
            transcript_word_byte_index_from(transcript, caption, split_source_us, &token_starts)
        }) {
            if !require_nonempty_halves || nonempty_text_halves(&caption.text, byte_index) {
                return Ok(byte_index);
            }
        }
        let token_index = proportional_index(
            split_timeline_us.0 - caption.range.start_us.0,
            caption.range.duration()?.0,
            token_starts.len(),
        )?;
        let byte_index = token_starts[token_index];
        if nonempty_text_halves(&caption.text, byte_index) {
            return Ok(byte_index);
        }
    }
    let character_starts = caption
        .text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if character_starts.len() >= 2 {
        let character_index = proportional_index(
            split_timeline_us.0 - caption.range.start_us.0,
            caption.range.duration()?.0,
            character_starts.len(),
        )?;
        let byte_index = character_starts[character_index];
        if nonempty_text_halves(&caption.text, byte_index) {
            return Ok(byte_index);
        }
    }
    Err(edit_error(
        VideoErrorCode::InvalidCaption,
        "caption text cannot be divided into two non-empty deterministic substrings",
        "captions.text",
    ))
}

#[derive(Clone, Copy, Debug)]
struct RegionTranslation {
    old: TimeRange,
    delta: i64,
}

fn reorder_scene(
    manifest: &mut VideoProjectManifest,
    scene_id: &str,
    to_index: usize,
) -> VideoResult<()> {
    let from_index = find_scene_index(manifest, scene_id)?;
    let scene_count = manifest.reviewed_scenes.len();
    if to_index >= scene_count {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            format!("reorder index {to_index} is outside 0..{scene_count}"),
            "operations.to_index",
        ));
    }
    if from_index == to_index {
        return Err(edit_error(
            VideoErrorCode::InvalidScene,
            "reorder operation must change the scene position",
            "operations.to_index",
        ));
    }

    let original_scenes = manifest.reviewed_scenes.clone();
    let original_ranges = original_scenes
        .iter()
        .map(scene_timeline_range)
        .collect::<VideoResult<Vec<_>>>()?;
    let prefix_duration = original_ranges
        .first()
        .map(|range| range.start_us.0)
        .unwrap_or(0);
    let suffix_start = original_ranges
        .last()
        .map(|range| range.end_us.0)
        .unwrap_or(manifest.timeline_duration_us.0);
    let inter_scene_durations = original_ranges
        .windows(2)
        .map(|pair| {
            pair[1]
                .start_us
                .0
                .checked_sub(pair[0].end_us.0)
                .ok_or_else(arithmetic_overflow)
        })
        .collect::<VideoResult<Vec<_>>>()?;

    let mut order = (0..scene_count).collect::<Vec<_>>();
    let moved = order.remove(from_index);
    order.insert(to_index, moved);

    let mut cursor = Microseconds(prefix_duration);
    let mut new_starts = BTreeMap::new();
    let mut new_gap_starts = Vec::with_capacity(inter_scene_durations.len());
    for (slot, original_index) in order.iter().copied().enumerate() {
        new_starts.insert(original_scenes[original_index].id.clone(), cursor);
        cursor = cursor.checked_add(original_scenes[original_index].timeline_duration_us)?;
        if let Some(gap_duration) = inter_scene_durations.get(slot).copied() {
            let gap_start = cursor;
            cursor = cursor.checked_add(Microseconds(gap_duration))?;
            new_gap_starts.push(gap_start);
        }
    }
    let expected_suffix_start = Microseconds(suffix_start);
    if cursor != expected_suffix_start {
        return Err(edit_error(
            VideoErrorCode::ArithmeticOverflow,
            "scene reorder changed the aggregate timeline duration",
            "reviewed_scenes",
        ));
    }

    let mut regions = Vec::new();
    if prefix_duration > 0 {
        regions.push(RegionTranslation {
            old: TimeRange::new(0, prefix_duration)?,
            delta: 0,
        });
    }
    for (index, scene) in original_scenes.iter().enumerate() {
        let new_start = new_starts[scene.id.as_str()];
        regions.push(RegionTranslation {
            old: original_ranges[index],
            delta: new_start
                .0
                .checked_sub(original_ranges[index].start_us.0)
                .ok_or_else(arithmetic_overflow)?,
        });
        if let Some(duration) = inter_scene_durations.get(index).copied() {
            if duration > 0 {
                let old_gap = TimeRange::new(
                    original_ranges[index].end_us.0,
                    original_ranges[index + 1].start_us.0,
                )?;
                regions.push(RegionTranslation {
                    old: old_gap,
                    delta: new_gap_starts[index]
                        .0
                        .checked_sub(old_gap.start_us.0)
                        .ok_or_else(arithmetic_overflow)?,
                });
            }
        }
    }
    if suffix_start < manifest.timeline_duration_us.0 {
        regions.push(RegionTranslation {
            old: TimeRange::new(suffix_start, manifest.timeline_duration_us.0)?,
            delta: 0,
        });
    }

    manifest.reviewed_scenes = order
        .into_iter()
        .map(|index| {
            let mut scene = original_scenes[index].clone();
            scene.timeline_start_us = new_starts[scene.id.as_str()];
            scene
        })
        .collect();
    for track in &mut manifest.tracks {
        for clip in &mut track.clips {
            clip.timeline_start_us = translated_range(clip.timeline_range()?, &regions)?.start_us;
        }
        track.clips.sort_by(|left, right| {
            (left.timeline_start_us, &left.id).cmp(&(right.timeline_start_us, &right.id))
        });
    }
    for gap in &mut manifest.gaps {
        gap.range = translated_range(gap.range, &regions)?;
    }
    manifest.gaps.sort_by(|left, right| {
        (&left.track_id, left.range.start_us, &left.id).cmp(&(
            &right.track_id,
            right.range.start_us,
            &right.id,
        ))
    });
    for caption in &mut manifest.captions {
        caption.range = translated_range(caption.range, &regions)?;
    }
    manifest.captions.sort_by(|left, right| {
        (left.range.start_us, &left.id).cmp(&(right.range.start_us, &right.id))
    });
    for layer in &mut manifest.visual_layers {
        layer.range = translated_range(layer.range, &regions)?;
    }
    manifest.visual_layers.sort_by(|left, right| {
        (left.range.start_us, left.z_index, &left.id).cmp(&(
            right.range.start_us,
            right.z_index,
            &right.id,
        ))
    });
    Ok(())
}

fn translated_range(range: TimeRange, regions: &[RegionTranslation]) -> VideoResult<TimeRange> {
    let matches = regions
        .iter()
        .filter(|region| range_contains(region.old, range))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(edit_error(
            VideoErrorCode::InvalidTrack,
            "timeline item crosses a scene or inter-scene boundary and cannot be reordered safely",
            "timeline",
        ));
    }
    checked_shift_range(range, matches[0].delta)
}

fn merge_scenes(
    manifest: &mut VideoProjectManifest,
    first_scene_id: &str,
    second_scene_id: &str,
) -> VideoResult<()> {
    let first_index = find_scene_index(manifest, first_scene_id)?;
    let second_index = find_scene_index(manifest, second_scene_id)?;
    if second_index != first_index + 1 {
        return Err(merge_error(
            "merge requires adjacent scenes in canonical timeline order",
            "operations.second_scene_id",
        ));
    }
    let first = manifest.reviewed_scenes[first_index].clone();
    let second = manifest.reviewed_scenes[second_index].clone();
    let first_timeline = scene_timeline_range(&first)?;
    let second_timeline = scene_timeline_range(&second)?;
    if first_timeline.end_us != second_timeline.start_us {
        return Err(merge_error(
            "merge requires contiguous sibling timeline ranges",
            "reviewed_scenes.timeline_start_us",
        ));
    }
    let boundary = first_timeline.end_us;
    if second.id != deterministic_split_id("scene", &first.id, boundary) {
        return Err(merge_error(
            "second scene identifier is not derived from the first split sibling",
            "operations.second_scene_id",
        ));
    }
    let (Some(first_source), Some(second_source)) = (first.source_range, second.source_range)
    else {
        return Err(merge_error(
            "merge requires source-backed split siblings",
            "reviewed_scenes.source_range",
        ));
    };
    if first.source_asset_id != second.source_asset_id
        || first.candidate_id != second.candidate_id
        || first_source.end_us != second_source.start_us
        || first.title != second.title
        || first.script != second.script
        || first.review_state != second.review_state
        || first.revision != second.revision
        || first.revision == 0
    {
        return Err(merge_error(
            "merge accepts only unchanged compatible split siblings",
            "reviewed_scenes",
        ));
    }
    if manifest.narration_bindings.iter().any(|binding| {
        matches!(
            binding.scene_id.as_deref(),
            Some(id) if id == first_scene_id || id == second_scene_id
        )
    }) || manifest.render_artifacts.iter().any(|artifact| {
        matches!(
            artifact.scene_id.as_deref(),
            Some(id) if id == first_scene_id || id == second_scene_id
        )
    }) {
        return Err(merge_error(
            "split-derived right scene acquired dependent artifacts and is no longer mergeable",
            "reviewed_scenes",
        ));
    }
    manifest.layout.elements = merge_split_layout_elements(
        &manifest.layout.elements,
        first_scene_id,
        second_scene_id,
        boundary,
    )?;

    let mut found_anchor_pair = false;
    for track in &mut manifest.tracks {
        let original_clips = track.clips.clone();
        let mut consumed = BTreeSet::new();
        let mut clips = Vec::with_capacity(original_clips.len());
        for (index, clip) in original_clips.iter().enumerate() {
            if consumed.contains(&index) {
                continue;
            }
            if clip.timeline_range()?.end_us == boundary {
                let expected_id = deterministic_split_id("clip", &clip.id, boundary);
                if let Some((right_index, right)) =
                    original_clips
                        .iter()
                        .enumerate()
                        .find(|(right_index, right)| {
                            !consumed.contains(right_index)
                                && right.id == expected_id
                                && right.timeline_start_us == boundary
                        })
                {
                    validate_merge_clip_pair(clip, right, first_scene_id, second_scene_id)?;
                    let mut merged = clip.clone();
                    merged.source_range =
                        TimeRange::new(clip.source_range.start_us.0, right.source_range.end_us.0)?;
                    merged.timeline_duration_us = Microseconds(
                        clip.timeline_duration_us
                            .0
                            .checked_add(right.timeline_duration_us.0)
                            .ok_or_else(arithmetic_overflow)?,
                    );
                    if clip.scene_id.as_deref() == Some(first_scene_id)
                        && right.scene_id.as_deref() == Some(second_scene_id)
                        && clip.timeline_range()? == first_timeline
                        && right.timeline_range()? == second_timeline
                        && clip.source_range == first_source
                        && right.source_range == second_source
                        && clip.media.source_asset_id == first.source_asset_id
                    {
                        found_anchor_pair = true;
                    }
                    consumed.insert(right_index);
                    clips.push(merged);
                    continue;
                }
            }
            let mut unchanged = clip.clone();
            if unchanged.scene_id.as_deref() == Some(second_scene_id) {
                unchanged.scene_id = Some(first_scene_id.to_string());
            }
            clips.push(unchanged);
        }
        track.clips = clips;
    }
    if !found_anchor_pair {
        return Err(merge_error(
            "merge requires a contiguous split-derived clip pair on the original source route",
            "tracks.clips",
        ));
    }

    manifest.gaps = merge_split_gaps(&manifest.gaps, boundary)?;
    manifest.captions = merge_split_captions(
        &manifest.captions,
        first_scene_id,
        second_scene_id,
        boundary,
    )?;
    for layer in &mut manifest.visual_layers {
        if layer.scene_id.as_deref() == Some(second_scene_id) {
            layer.scene_id = Some(first_scene_id.to_string());
        }
    }

    let mut merged_scene = first.clone();
    merged_scene.source_range = Some(TimeRange::new(
        first_source.start_us.0,
        second_source.end_us.0,
    )?);
    merged_scene.timeline_duration_us = Microseconds(
        first
            .timeline_duration_us
            .0
            .checked_add(second.timeline_duration_us.0)
            .ok_or_else(arithmetic_overflow)?,
    );
    merged_scene.revision = first.revision - 1;
    manifest
        .reviewed_scenes
        .splice(first_index..=second_index, [merged_scene]);
    Ok(())
}

fn validate_merge_clip_pair(
    left: &TimelineClip,
    right: &TimelineClip,
    first_scene_id: &str,
    second_scene_id: &str,
) -> VideoResult<()> {
    let scene_pair = (left.scene_id.as_deref(), right.scene_id.as_deref());
    if !matches!(
        scene_pair,
        (Some(left_id), Some(right_id))
            if left_id == first_scene_id && right_id == second_scene_id
    ) && scene_pair != (None, None)
    {
        return Err(merge_error(
            "split clip pair has incompatible scene ownership",
            "tracks.clips.scene_id",
        ));
    }
    if left.timeline_range()?.end_us != right.timeline_start_us
        || left.source_range.end_us != right.source_range.start_us
        || left.media != right.media
        || left.playback_rate != right.playback_rate
        || left.gain_db_milli != right.gain_db_milli
        || left.muted != right.muted
        || left.crop != right.crop
    {
        return Err(merge_error(
            "split clip pair no longer has one contiguous media/playback route",
            "tracks.clips",
        ));
    }
    Ok(())
}

fn merge_split_layout_elements(
    elements: &[super::contracts::LayoutElement],
    first_scene_id: &str,
    second_scene_id: &str,
    boundary: Microseconds,
) -> VideoResult<Vec<super::contracts::LayoutElement>> {
    let first_elements = elements
        .iter()
        .filter(|element| element.scene_id.as_deref() == Some(first_scene_id))
        .collect::<Vec<_>>();
    let mut split_element_ids = BTreeSet::new();
    for source in first_elements {
        let expected_id = deterministic_split_id("layout", &source.id, boundary);
        let Some(split) = elements.iter().find(|element| element.id == expected_id) else {
            return Err(merge_error(
                "split-derived right scene is missing its paired layout element",
                "layout.elements",
            ));
        };
        if split.scene_id.as_deref() != Some(second_scene_id)
            || source.role != split.role
            || source.bounds != split.bounds
            || source.z_index != split.z_index
            || source.style_id != split.style_id
        {
            return Err(merge_error(
                "split-derived layout siblings are no longer compatible",
                "layout.elements",
            ));
        }
        split_element_ids.insert(split.id.as_str());
    }
    if elements.iter().any(|element| {
        element.scene_id.as_deref() == Some(second_scene_id)
            && !split_element_ids.contains(element.id.as_str())
    }) {
        return Err(merge_error(
            "right sibling has a layout element that was not created by the split",
            "layout.elements",
        ));
    }
    Ok(elements
        .iter()
        .filter(|element| !split_element_ids.contains(element.id.as_str()))
        .cloned()
        .collect())
}

fn merge_split_gaps(gaps: &[TimelineGap], boundary: Microseconds) -> VideoResult<Vec<TimelineGap>> {
    let mut consumed = BTreeSet::new();
    let mut merged = Vec::with_capacity(gaps.len());
    for (index, gap) in gaps.iter().enumerate() {
        if consumed.contains(&index) {
            continue;
        }
        if gap.range.end_us == boundary {
            let expected_id = deterministic_split_id("gap", &gap.id, boundary);
            if let Some((right_index, right)) =
                gaps.iter().enumerate().find(|(right_index, right)| {
                    !consumed.contains(right_index)
                        && right.id == expected_id
                        && right.range.start_us == boundary
                })
            {
                if gap.track_id != right.track_id
                    || gap.reason != right.reason
                    || gap.source_asset_id != right.source_asset_id
                {
                    return Err(merge_error(
                        "split gap pair is no longer compatible",
                        "gaps",
                    ));
                }
                let source_range = match (gap.source_range, right.source_range) {
                    (Some(left), Some(right)) if left.end_us == right.start_us => {
                        Some(TimeRange::new(left.start_us.0, right.end_us.0)?)
                    }
                    (None, None) => None,
                    _ => {
                        return Err(merge_error(
                            "split source-clock gap pair is no longer contiguous",
                            "gaps.source_range",
                        ))
                    }
                };
                let mut combined = gap.clone();
                combined.range = TimeRange::new(gap.range.start_us.0, right.range.end_us.0)?;
                combined.source_range = source_range;
                consumed.insert(right_index);
                merged.push(combined);
                continue;
            }
        }
        merged.push(gap.clone());
    }
    Ok(merged)
}

fn merge_split_captions(
    captions: &[CaptionCue],
    first_scene_id: &str,
    second_scene_id: &str,
    boundary: Microseconds,
) -> VideoResult<Vec<CaptionCue>> {
    let mut consumed = BTreeSet::new();
    let mut merged = Vec::with_capacity(captions.len());
    for (index, caption) in captions.iter().enumerate() {
        if consumed.contains(&index) {
            continue;
        }
        if caption.range.end_us == boundary {
            let expected_id = deterministic_split_id("caption", &caption.id, boundary);
            if let Some((right_index, right)) =
                captions.iter().enumerate().find(|(right_index, right)| {
                    !consumed.contains(right_index)
                        && right.id == expected_id
                        && right.range.start_us == boundary
                })
            {
                let scene_pair = (caption.scene_id.as_deref(), right.scene_id.as_deref());
                if !matches!(
                    scene_pair,
                    (Some(left_id), Some(right_id))
                        if left_id == first_scene_id && right_id == second_scene_id
                ) && scene_pair != (None, None)
                {
                    return Err(merge_error(
                        "split caption pair has incompatible scene ownership",
                        "captions.scene_id",
                    ));
                }
                if caption.style_id != right.style_id
                    || caption.speaker_id != right.speaker_id
                    || caption.transcript_segment_id != right.transcript_segment_id
                {
                    return Err(merge_error(
                        "split caption pair is no longer compatible",
                        "captions",
                    ));
                }
                let mut combined = caption.clone();
                combined.range = TimeRange::new(caption.range.start_us.0, right.range.end_us.0)?;
                combined.text.push_str(&right.text);
                combined.validate()?;
                consumed.insert(right_index);
                merged.push(combined);
                continue;
            }
        }
        let mut unchanged = caption.clone();
        if unchanged.scene_id.as_deref() == Some(second_scene_id) {
            unchanged.scene_id = Some(first_scene_id.to_string());
        }
        merged.push(unchanged);
    }
    Ok(merged)
}

fn merge_error(message: impl Into<String>, field: impl Into<String>) -> VideoError {
    edit_error(VideoErrorCode::InvalidScene, message, field)
}

#[cfg(test)]
mod tests {
    use super::super::contracts::TakeFidelity;
    use super::super::contracts::{
        AudioMix, AudioMixTrack, CandidateStatus, CanvasMode, CanvasSpec, ClipCandidate, GapReason,
        LayoutElement, LayoutPlan, LayoutRole, MediaProbe, MediaReference, NarrationBinding,
        NormalizedRect, Provenance, ProvenanceKind, PublicationState, RationalFrameRate,
        RationalRate, RenderArtifact, RenderArtifactRole, ReviewState, SourceAsset,
        SourceAssetKind, TimelineTrack, TrackKind, TranscriptSegment, TranscriptTimingSource,
        TranscriptVersion, TranscriptWord,
    };
    use super::super::visuals::{
        VisualAsset, VisualEasing, VisualFit, VisualLayer, VisualMimeType, VisualMotion,
    };
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    const NOW: &str = "2026-08-28T12:00:00Z";
    const SOURCE_ID: &str = "source-main";
    const FIRST_SCENE_ID: &str = "scene-first";
    const SECOND_SCENE_ID: &str = "scene-second";

    fn range(start: i64, end: i64) -> TimeRange {
        TimeRange::new(start, end).unwrap()
    }

    fn clip(
        id: &str,
        scene_id: &str,
        source_start_us: i64,
        source_end_us: i64,
        timeline_start_us: i64,
        timeline_end_us: i64,
    ) -> TimelineClip {
        TimelineClip {
            id: id.into(),
            scene_id: Some(scene_id.into()),
            turn_id: None,
            media: MediaReference {
                source_asset_id: Some(SOURCE_ID.into()),
                render_artifact_id: None,
            },
            source_range: range(source_start_us, source_end_us),
            timeline_start_us: Microseconds(timeline_start_us),
            timeline_duration_us: Microseconds(timeline_end_us - timeline_start_us),
            playback_rate: RationalRate::ONE,
            gain_db_milli: 0,
            muted: false,
            crop: None,
        }
    }

    fn manifest() -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "project-editor",
            "Editor fixture",
            RationalFrameRate::FPS_30,
            Microseconds(7_000_000),
            LayoutPlan {
                mode: CanvasMode::Portrait,
                canvas: CanvasSpec {
                    width: 1080,
                    height: 1920,
                    pixel_aspect_numerator: 1,
                    pixel_aspect_denominator: 1,
                },
                safe_area: NormalizedRect {
                    x_bp: 0,
                    y_bp: 0,
                    width_bp: 10_000,
                    height_bp: 10_000,
                },
                background_rgba: [0, 0, 0, 255],
                elements: Vec::new(),
            },
            AudioMix {
                target_lufs_milli: -16_000,
                true_peak_db_milli: -1_000,
                tracks: Vec::new(),
            },
            NOW,
        )
        .unwrap();
        manifest.source_assets.push(SourceAsset {
            id: SOURCE_ID.into(),
            kind: SourceAssetKind::LocalVideo,
            managed_path: "sources/main.mp4".into(),
            sha256: "a".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(20_000_000),
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(RationalFrameRate::FPS_30),
                has_video: true,
                has_audio: true,
                format_name: "mov,mp4".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: NOW.into(),
                producer: "editor-test".into(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        });
        let words = vec![
            ("word-one", 1_100_000, 1_500_000, "one"),
            ("word-two", 1_600_000, 2_200_000, "two"),
            ("word-three", 2_400_000, 3_100_000, "three"),
            ("word-four", 3_200_000, 3_800_000, "four"),
            ("word-five", 8_200_000, 9_100_000, "five"),
            ("word-six", 9_300_000, 10_400_000, "six"),
        ]
        .into_iter()
        .map(|(id, start, end, text)| TranscriptWord {
            id: id.into(),
            range: range(start, end),
            text: text.into(),
            speaker_id: None,
            confidence_milli: Some(950),
        })
        .collect::<Vec<_>>();
        manifest.transcript = Some(TranscriptVersion {
            id: "transcript-main".into(),
            source_asset_id: SOURCE_ID.into(),
            source_clock_duration_us: Microseconds(20_000_000),
            language: Some("en".into()),
            timing_source: TranscriptTimingSource::SoundArWhisper,
            preserved_source_gaps: true,
            segments: vec![
                TranscriptSegment {
                    id: "segment-first".into(),
                    range: range(1_000_000, 4_000_000),
                    text: "one two three four".into(),
                    speaker_id: None,
                    word_ids: vec![
                        "word-one".into(),
                        "word-two".into(),
                        "word-three".into(),
                        "word-four".into(),
                    ],
                },
                TranscriptSegment {
                    id: "segment-second".into(),
                    range: range(8_000_000, 11_000_000),
                    text: "five six".into(),
                    speaker_id: None,
                    word_ids: vec!["word-five".into(), "word-six".into()],
                },
            ],
            words,
            content_sha256: "b".repeat(64),
            created_at: NOW.into(),
        });
        manifest.candidates = vec![
            ClipCandidate {
                id: "candidate-first".into(),
                source_asset_id: SOURCE_ID.into(),
                source_range: range(1_000_000, 4_000_000),
                title: "First".into(),
                rationale: "First source selection".into(),
                transcript_segment_ids: vec!["segment-first".into()],
                score_milli: 900,
                status: CandidateStatus::Accepted,
            },
            ClipCandidate {
                id: "candidate-second".into(),
                source_asset_id: SOURCE_ID.into(),
                source_range: range(8_000_000, 11_000_000),
                title: "Second".into(),
                rationale: "Second source selection".into(),
                transcript_segment_ids: vec!["segment-second".into()],
                score_milli: 850,
                status: CandidateStatus::Accepted,
            },
        ];
        manifest.reviewed_scenes = vec![
            ReviewedScene {
                id: FIRST_SCENE_ID.into(),
                candidate_id: Some("candidate-first".into()),
                source_asset_id: Some(SOURCE_ID.into()),
                source_range: Some(range(1_000_000, 4_000_000)),
                timeline_start_us: Microseconds::ZERO,
                timeline_duration_us: Microseconds(3_000_000),
                title: "First".into(),
                script: "one two three four".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
            ReviewedScene {
                id: SECOND_SCENE_ID.into(),
                candidate_id: Some("candidate-second".into()),
                source_asset_id: Some(SOURCE_ID.into()),
                source_range: Some(range(8_000_000, 11_000_000)),
                timeline_start_us: Microseconds(4_000_000),
                timeline_duration_us: Microseconds(3_000_000),
                title: "Second".into(),
                script: "five six".into(),
                review_state: ReviewState::Approved,
                revision: 1,
            },
        ];
        manifest.tracks = vec![
            TimelineTrack {
                id: "video-main".into(),
                kind: TrackKind::Video,
                clips: vec![
                    clip(
                        "clip-video-first",
                        FIRST_SCENE_ID,
                        1_000_000,
                        4_000_000,
                        0,
                        3_000_000,
                    ),
                    clip(
                        "clip-video-second",
                        SECOND_SCENE_ID,
                        8_000_000,
                        11_000_000,
                        4_000_000,
                        7_000_000,
                    ),
                ],
                preserve_gaps: true,
            },
            TimelineTrack {
                id: "audio-main".into(),
                kind: TrackKind::Audio,
                clips: vec![
                    clip(
                        "clip-audio-first",
                        FIRST_SCENE_ID,
                        1_000_000,
                        4_000_000,
                        0,
                        3_000_000,
                    ),
                    clip(
                        "clip-audio-second",
                        SECOND_SCENE_ID,
                        8_000_000,
                        11_000_000,
                        4_000_000,
                        7_000_000,
                    ),
                ],
                preserve_gaps: true,
            },
            TimelineTrack {
                id: "overlay-empty".into(),
                kind: TrackKind::Overlay,
                clips: Vec::new(),
                preserve_gaps: true,
            },
        ];
        manifest.gaps = vec![
            TimelineGap {
                id: "gap-video-transition".into(),
                track_id: "video-main".into(),
                range: range(3_000_000, 4_000_000),
                reason: GapReason::Transition,
                source_asset_id: None,
                source_range: None,
            },
            TimelineGap {
                id: "gap-audio-transition".into(),
                track_id: "audio-main".into(),
                range: range(3_000_000, 4_000_000),
                reason: GapReason::Editorial,
                source_asset_id: None,
                source_range: None,
            },
            TimelineGap {
                id: "gap-overlay-first".into(),
                track_id: "overlay-empty".into(),
                range: range(0, 3_000_000),
                reason: GapReason::Padding,
                source_asset_id: None,
                source_range: None,
            },
            TimelineGap {
                id: "gap-overlay-transition".into(),
                track_id: "overlay-empty".into(),
                range: range(3_000_000, 4_000_000),
                reason: GapReason::Transition,
                source_asset_id: None,
                source_range: None,
            },
            TimelineGap {
                id: "gap-overlay-second".into(),
                track_id: "overlay-empty".into(),
                range: range(4_000_000, 7_000_000),
                reason: GapReason::Padding,
                source_asset_id: None,
                source_range: None,
            },
        ];
        manifest.captions = vec![
            CaptionCue {
                id: "caption-first".into(),
                range: range(100_000, 2_900_000),
                text: "one two three four".into(),
                style_id: "caption-clean-white".into(),
                speaker_id: None,
                transcript_segment_id: Some("segment-first".into()),
                scene_id: Some(FIRST_SCENE_ID.into()),
            },
            CaptionCue {
                id: "caption-second".into(),
                range: range(4_100_000, 6_900_000),
                text: "five six".into(),
                style_id: "caption-clean-white".into(),
                speaker_id: None,
                transcript_segment_id: Some("segment-second".into()),
                scene_id: Some(SECOND_SCENE_ID.into()),
            },
        ];
        manifest.layout.elements.push(LayoutElement {
            id: "layout-first".into(),
            role: LayoutRole::PrimaryVideo,
            scene_id: Some(FIRST_SCENE_ID.into()),
            bounds: NormalizedRect {
                x_bp: 0,
                y_bp: 0,
                width_bp: 10_000,
                height_bp: 10_000,
            },
            z_index: 0,
            style_id: None,
        });
        manifest.audio_mix.tracks.push(AudioMixTrack {
            track_id: "audio-main".into(),
            gain_db_milli: 0,
            pan_milli: 0,
            ducking: None,
        });
        manifest.validate_strict().unwrap();
        manifest
    }

    fn add_visual_layer(manifest: &mut VideoProjectManifest, range: TimeRange, scene_id: &str) {
        manifest.visual_assets.push(VisualAsset {
            id: "visual-editor".into(),
            managed_path: "visuals/editor.png".into(),
            sha256: "e".repeat(64),
            mime_type: VisualMimeType::Png,
            width: 1600,
            height: 900,
            has_alpha: false,
            size_bytes: 1_024,
            provenance: Provenance {
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: NOW.into(),
                producer: "editor-image-test".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::new(),
            },
            created_at: NOW.into(),
        });
        manifest.visual_layers.push(VisualLayer {
            id: "visual-layer-editor".into(),
            asset_id: "visual-editor".into(),
            scene_id: Some(scene_id.into()),
            range,
            fit: VisualFit::Contain,
            crop: None,
            z_index: 2,
            motion: VisualMotion {
                start_bounds: NormalizedRect {
                    x_bp: 1_000,
                    y_bp: 2_000,
                    width_bp: 8_000,
                    height_bp: 4_500,
                },
                end_bounds: NormalizedRect {
                    x_bp: 200,
                    y_bp: 1_500,
                    width_bp: 9_600,
                    height_bp: 5_400,
                },
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: VisualEasing::EaseInOut,
            },
            transition_in_us: Microseconds(100_000),
            transition_out_us: Microseconds(100_000),
        });
    }

    /// Add a two-character exchange to the editor fixture so beat edits have real turns.
    fn with_dialogue(manifest: &VideoProjectManifest) -> VideoProjectManifest {
        use super::super::cast::{CastDelivery, CastMember, DialogueTurn};
        let mut manifest = manifest.clone();
        for (id, name, voice) in [
            ("narrator", "NARRATOR", "af-heart"),
            ("adaeze", "ADAEZE", "af-bella"),
        ] {
            manifest.cast.push(CastMember {
                id: id.into(),
                name: name.into(),
                display_name: name.into(),
                voice_id: voice.into(),
                model_id: "hexgrad/Kokoro-82M".into(),
                language: "en-US".into(),
                delivery: CastDelivery::default(),
                consent_reference_id: None,
                notes: None,
                created_at: NOW.into(),
            });
        }
        for (index, (id, character, text)) in [
            ("turn-a", "narrator", "He asked her name."),
            ("turn-b", "adaeze", "Adaeze."),
        ]
        .into_iter()
        .enumerate()
        {
            manifest.dialogue.push(DialogueTurn {
                id: id.into(),
                scene_id: None,
                order: index as u32,
                character_id: character.into(),
                text: text.into(),
                direction: None,
                source_line: index as u32 + 1,
                revision: 1,
            });
        }
        manifest.turn_beats =
            derive_turn_beats(&manifest.dialogue, &manifest.performance_clock, &[]).unwrap();
        manifest.validate_strict().unwrap();
        manifest
    }

    #[test]
    fn holding_a_beat_marks_it_explicit_and_clearing_it_restores_the_derived_one() {
        let manifest = with_dialogue(&manifest());
        let derived = manifest.turn_beats[1].lead_in_us;

        let held = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetTurnBeat {
                turn_id: "turn-b".into(),
                lead_in_us: Microseconds(2_000_000),
                overlap_us: Microseconds::ZERO,
            }]),
        )
        .unwrap();
        let beat = held
            .manifest
            .turn_beats
            .iter()
            .find(|beat| beat.turn_id == "turn-b")
            .unwrap();
        assert_eq!(beat.lead_in_us, Microseconds(2_000_000));
        assert_eq!(beat.source, BeatSource::Explicit);
        assert!(held
            .receipt
            .invalidated_stages
            .contains(&RevisionStage::SceneRender));
        assert!(
            !held
                .receipt
                .invalidated_stages
                .contains(&RevisionStage::Speech),
            "retiming a pause must not re-read any line"
        );

        let cleared = apply_timeline_edit(
            &held.manifest,
            &request(vec![VideoTimelineOperation::ClearTurnBeat {
                turn_id: "turn-b".into(),
            }]),
        )
        .unwrap();
        let restored = cleared
            .manifest
            .turn_beats
            .iter()
            .find(|beat| beat.turn_id == "turn-b")
            .unwrap();
        assert_eq!(restored.lead_in_us, derived);
        assert_eq!(restored.source, BeatSource::Derived);
    }

    /// Give both turns a published take so a rule change has something to stale.
    fn with_takes(manifest: &VideoProjectManifest) -> VideoProjectManifest {
        use super::super::contracts::{
            NarrationBinding, PublicationState, RenderArtifact, RenderArtifactRole,
        };
        use sha2::{Digest, Sha256};

        let mut manifest = manifest.clone();
        for (index, turn) in manifest.dialogue.clone().iter().enumerate() {
            let artifact_id = format!("take-{}", turn.id);
            manifest.render_artifacts.push(RenderArtifact {
                id: artifact_id.clone(),
                role: RenderArtifactRole::SceneSegment,
                scene_id: None,
                managed_path: format!("renders/{artifact_id}.wav"),
                sha256: format!("{index:064}"),
                cache_key: format!("{:064}", index + 100),
                mime_type: "audio/wav".into(),
                duration_us: Microseconds(1_500_000).into(),
                width: None,
                height: None,
                publication_state: PublicationState::Published,
                created_at: NOW.into(),
            });
            let member = manifest
                .cast
                .iter()
                .find(|member| member.id == turn.character_id)
                .unwrap()
                .clone();
            manifest.narration_bindings.push(NarrationBinding {
                id: format!("binding-{}", turn.id),
                scene_id: None,
                turn_id: Some(turn.id.clone()),
                character_id: Some(turn.character_id.clone()),
                lexicon_fingerprint: fingerprint_for_character(&manifest.lexicon, &member.id),
                fidelity: TakeFidelity::Final,
                render_artifact_id: artifact_id,
                history_id: format!("history-{}", turn.id),
                generation_job_id: format!("job-{}", turn.id),
                voice_id: member.voice_id.clone(),
                model_id: member.model_id.clone(),
                speaker: member.name.clone(),
                language: member.language.clone(),
                script_sha256: format!("{:x}", Sha256::digest(turn.text.as_bytes())),
                created_at: NOW.into(),
            });
        }
        manifest.validate_strict().unwrap();
        manifest
    }

    fn rule(
        id: &str,
        scope: super::super::lexicon::LexiconScope,
        character: Option<&str>,
    ) -> LexiconEntry {
        use super::super::lexicon::LexiconMatch;
        LexiconEntry {
            id: id.into(),
            scope,
            character_id: character.map(str::to_string),
            match_text: "Adaeze".into(),
            replacement: "Ah-DAH-eh-zeh".into(),
            matching: LexiconMatch::Word,
            notes: None,
            created_at: NOW.into(),
        }
    }

    #[test]
    fn a_character_rule_drops_only_that_characters_takes() {
        use super::super::lexicon::LexiconScope;

        let manifest = with_takes(&with_dialogue(&manifest()));
        assert_eq!(manifest.narration_bindings.len(), 2);

        let edited = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetLexiconEntry {
                entry: rule("rule-adaeze", LexiconScope::Character, Some("adaeze")),
            }]),
        )
        .unwrap();

        let surviving = edited
            .manifest
            .narration_bindings
            .iter()
            .map(|binding| binding.turn_id.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            surviving,
            vec!["turn-a".to_string()],
            "only ADAEZE's line is re-read"
        );
        assert!(edited
            .receipt
            .invalidated_stages
            .contains(&RevisionStage::Speech));
    }

    #[test]
    fn a_project_rule_drops_every_take_and_removing_it_drops_them_again() {
        use super::super::lexicon::LexiconScope;

        let manifest = with_takes(&with_dialogue(&manifest()));
        let applied = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetLexiconEntry {
                entry: rule("rule-project", LexiconScope::Project, None),
            }]),
        )
        .unwrap();
        assert!(applied.manifest.narration_bindings.is_empty());

        // Re-record both takes under the new rules, then prove removing the rule stales them too.
        let mut rerecorded = applied.manifest.clone();
        rerecorded.narration_bindings = manifest
            .narration_bindings
            .iter()
            .map(|binding| {
                let mut binding = binding.clone();
                binding.lexicon_fingerprint =
                    fingerprint_for_character(&rerecorded.lexicon, "narrator");
                binding
            })
            .collect();
        rerecorded.validate_strict().unwrap();

        let removed = apply_timeline_edit(
            &rerecorded,
            &request(vec![VideoTimelineOperation::RemoveLexiconEntry {
                entry_id: "rule-project".into(),
            }]),
        )
        .unwrap();
        assert!(removed.manifest.lexicon.is_empty());
        assert!(removed.manifest.narration_bindings.is_empty());
    }

    #[test]
    fn removing_a_rule_that_does_not_exist_is_rejected() {
        let manifest = with_dialogue(&manifest());
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::RemoveLexiconEntry {
                entry_id: "rule-absent".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    /// Place a narration take on its own audio track and add an empty music track beside it, so a
    /// bed cue has something real to duck against and somewhere real to sit.
    fn with_music_track(manifest: &VideoProjectManifest) -> VideoProjectManifest {
        use super::super::contracts::{MediaReference, RationalRate, TimelineTrack};

        let mut manifest = with_takes(manifest);
        let narration_artifact = manifest.narration_bindings[0].render_artifact_id.clone();
        manifest.tracks.push(TimelineTrack {
            id: "speech".into(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: vec![TimelineClip {
                id: "speech-clip".into(),
                scene_id: None,
                turn_id: manifest.narration_bindings[0].turn_id.clone(),
                media: MediaReference {
                    source_asset_id: None,
                    render_artifact_id: Some(narration_artifact),
                },
                source_range: range(0, 1_500_000),
                timeline_start_us: Microseconds::ZERO,
                timeline_duration_us: Microseconds(1_500_000),
                playback_rate: RationalRate {
                    numerator: 1,
                    denominator: 1,
                },
                gain_db_milli: 0,
                muted: false,
                crop: None,
            }],
        });
        manifest.tracks.push(TimelineTrack {
            id: "music".into(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: Vec::new(),
        });
        manifest.validate_strict().unwrap();
        manifest
    }

    /// A registered soundAr music artifact for a cue to point at.
    fn music_asset() -> super::super::contracts::SourceAsset {
        use super::super::contracts::{
            MediaProbe, Provenance, ProvenanceKind, SourceAsset, SourceAssetKind,
        };
        SourceAsset {
            id: "music-one".into(),
            kind: SourceAssetKind::SoundArMusic,
            managed_path: "sources/music-one.wav".into(),
            sha256: "c".repeat(64),
            probe: MediaProbe {
                duration_us: Microseconds(45_000_000),
                width: None,
                height: None,
                frame_rate: None,
                has_video: false,
                has_audio: true,
                format_name: "wav".into(),
            },
            provenance: Provenance {
                kind: ProvenanceKind::UserUpload,
                original_uri: None,
                imported_at: NOW.into(),
                producer: "editor-test".into(),
                producer_version: None,
                metadata: BTreeMap::new(),
            },
            rights_confirmation_id: None,
        }
    }

    fn bed_cue(track_id: Option<&str>) -> MusicCue {
        use super::super::score::{CueAnchor, CueRole};
        MusicCue {
            id: "cue-bed".into(),
            role: CueRole::Bed,
            anchor: CueAnchor::Turn {
                turn_id: "turn-a".into(),
            },
            target_duration_us: Microseconds(30_000_000),
            direction: "warm, restrained, low strings".into(),
            source_asset_id: track_id.map(|_| "music-one".into()),
            track_id: track_id.map(str::to_string),
            gain_db_milli: -9_000,
            fade_in_us: Microseconds(500_000),
            fade_out_us: Microseconds(1_000_000),
            created_at: NOW.into(),
        }
    }

    #[test]
    fn a_bed_placed_on_a_track_is_given_its_ducking_envelope() {
        use super::super::score::DEFAULT_BED_DUCK_DB_MILLI;

        let mut manifest = with_music_track(&with_dialogue(&manifest()));
        manifest.source_assets.push(music_asset());
        manifest.validate_strict().unwrap();

        let edited = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetMusicCue {
                cue: bed_cue(Some("music")),
            }]),
        )
        .unwrap();

        let mix = edited
            .manifest
            .audio_mix
            .tracks
            .iter()
            .find(|mix| mix.track_id == "music")
            .expect("the bed has a mix entry");
        let ducking = mix.ducking.as_ref().expect("a bed always ducks");
        assert_eq!(ducking.sidechain_track_id, "speech");
        assert_eq!(ducking.reduction_db_milli, DEFAULT_BED_DUCK_DB_MILLI);
        assert_eq!(mix.gain_db_milli, -9_000);

        // Removing the cue takes its mix entry with it, so no envelope is left pointing at music
        // the project no longer has.
        let removed = apply_timeline_edit(
            &edited.manifest,
            &request(vec![VideoTimelineOperation::RemoveMusicCue {
                cue_id: "cue-bed".into(),
            }]),
        )
        .unwrap();
        assert!(removed.manifest.music_cues.is_empty());
        assert!(!removed
            .manifest
            .audio_mix
            .tracks
            .iter()
            .any(|mix| mix.track_id == "music"));
    }

    #[test]
    fn a_bed_cannot_be_placed_without_narration_to_duck_against() {
        use super::super::contracts::TimelineTrack;

        let mut manifest = with_dialogue(&manifest());
        manifest.source_assets.push(music_asset());
        manifest.tracks.push(TimelineTrack {
            id: "music".into(),
            kind: TrackKind::Audio,
            preserve_gaps: false,
            clips: Vec::new(),
        });
        manifest.validate_strict().unwrap();

        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetMusicCue {
                cue: bed_cue(Some("music")),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCue);
    }

    #[test]
    fn placing_a_cue_fits_it_anchors_it_and_gives_a_bed_its_envelope() {
        use super::super::score::CueAnchor;

        let mut manifest = with_music_track(&with_dialogue(&manifest()));
        manifest.source_assets.push(music_asset());
        // A bed anchored to the first narrated turn, shorter than the 45s generated piece.
        let mut cue = bed_cue(None);
        cue.anchor = CueAnchor::Turn {
            turn_id: "turn-a".into(),
        };
        cue.target_duration_us = Microseconds(1_000_000);
        cue.fade_in_us = Microseconds(200_000);
        cue.fade_out_us = Microseconds(200_000);
        manifest.music_cues.push(cue);
        manifest.validate_strict().unwrap();

        let placed = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PlaceMusicCue {
                cue_id: "cue-bed".into(),
                source_asset_id: "music-one".into(),
            }]),
        )
        .unwrap();

        let cue = &placed.manifest.music_cues[0];
        assert_eq!(cue.source_asset_id.as_deref(), Some("music-one"));
        assert_eq!(cue.track_id.as_deref(), Some("music-cue-bed"));
        assert!(!cue.needs_generation());

        let track = placed
            .manifest
            .tracks
            .iter()
            .find(|track| track.id == "music-cue-bed")
            .expect("the cue has a timeline track");
        assert!(matches!(track.kind, TrackKind::Audio));
        // The generated piece is trimmed to the cue's target and starts at its anchored turn.
        assert_eq!(track.clips[0].timeline_duration_us, Microseconds(1_000_000));
        assert_eq!(track.clips[0].timeline_start_us, Microseconds::ZERO);

        let ducking = placed
            .manifest
            .audio_mix
            .tracks
            .iter()
            .find(|mix| mix.track_id == "music-cue-bed")
            .and_then(|mix| mix.ducking.clone())
            .expect("a placed bed always ducks");
        assert_eq!(ducking.sidechain_track_id, "speech");

        // Placing the same cue twice would put its music on the timeline a second time.
        let error = apply_timeline_edit(
            &placed.manifest,
            &request(vec![VideoTimelineOperation::PlaceMusicCue {
                cue_id: "cue-bed".into(),
                source_asset_id: "music-one".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCue);
    }

    #[test]
    fn a_cue_whose_music_is_too_short_is_reported_rather_than_placed() {
        use super::super::score::CueAnchor;

        let mut manifest = with_music_track(&with_dialogue(&manifest()));
        manifest.source_assets.push(music_asset());
        let mut cue = bed_cue(None);
        cue.anchor = CueAnchor::Turn {
            turn_id: "turn-a".into(),
        };
        // The registered music is 45 seconds; this cue asks for two minutes.
        cue.target_duration_us = Microseconds(120_000_000);
        manifest.music_cues.push(cue);
        manifest.validate_strict().unwrap();

        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PlaceMusicCue {
                cue_id: "cue-bed".into(),
                source_asset_id: "music-one".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::CueFitFailed);
    }

    #[test]
    fn a_cue_can_only_be_placed_from_registered_music() {
        use super::super::score::CueAnchor;

        let mut manifest = with_music_track(&with_dialogue(&manifest()));
        let mut cue = bed_cue(None);
        cue.anchor = CueAnchor::Turn {
            turn_id: "turn-a".into(),
        };
        cue.target_duration_us = Microseconds(1_000_000);
        cue.fade_in_us = Microseconds(200_000);
        cue.fade_out_us = Microseconds(200_000);
        manifest.music_cues.push(cue);

        // The project's only other source is video, not soundAr music.
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PlaceMusicCue {
                cue_id: "cue-bed".into(),
                source_asset_id: SOURCE_ID.into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCue);
    }

    #[test]
    fn an_outro_cannot_be_placed_before_there_is_narration_to_follow() {
        use super::super::score::{CueAnchor, CueRole};

        let mut manifest = with_dialogue(&manifest());
        manifest.source_assets.push(music_asset());
        let mut outro = bed_cue(None);
        outro.id = "cue-outro".into();
        outro.role = CueRole::Outro;
        outro.anchor = CueAnchor::AfterFinalTurn;
        outro.target_duration_us = Microseconds(1_000_000);
        outro.fade_in_us = Microseconds(200_000);
        outro.fade_out_us = Microseconds(200_000);
        manifest.music_cues.push(outro);
        manifest.validate_strict().unwrap();

        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PlaceMusicCue {
                cue_id: "cue-outro".into(),
                source_asset_id: "music-one".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn a_planned_cue_needs_no_track_and_no_mix_entry() {
        let manifest = with_dialogue(&manifest());
        let edited = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetMusicCue {
                cue: bed_cue(None),
            }]),
        )
        .unwrap();
        assert_eq!(edited.manifest.music_cues.len(), 1);
        assert!(edited.manifest.music_cues[0].needs_generation());
        // Nothing is placed in the mix until there is music to place.
        assert_eq!(edited.manifest.audio_mix.tracks, manifest.audio_mix.tracks);
    }

    #[test]
    fn removing_a_cue_that_does_not_exist_is_rejected() {
        let manifest = with_dialogue(&manifest());
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::RemoveMusicCue {
                cue_id: "cue-absent".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    fn sound_asset() -> super::super::sound::SoundAsset {
        use super::super::sound::SoundAsset;
        SoundAsset {
            id: "sound-tone".into(),
            name: "Quiet room".into(),
            source_asset_id: "music-one".into(),
            tags: vec!["room tone".into()],
            created_at: NOW.into(),
        }
    }

    fn room_tone_layer(scene_id: &str, start: i64, end: i64) -> SoundLayer {
        use super::super::sound::SoundPlacementKind;
        SoundLayer {
            id: "tone-one".into(),
            asset_id: "sound-tone".into(),
            kind: SoundPlacementKind::RoomTone,
            scene_id: Some(scene_id.into()),
            turn_id: None,
            range: range(start, end),
            gain_db_milli: -26_000,
            fade_in_us: Microseconds(250_000),
            fade_out_us: Microseconds(250_000),
            loop_to_fill: true,
            duck_under_speech: false,
        }
    }

    #[test]
    fn room_tone_runs_under_a_whole_scene_and_can_be_removed() {
        let mut manifest = manifest();
        manifest.source_assets.push(music_asset());
        manifest.sound_assets.push(sound_asset());
        manifest.validate_strict().unwrap();
        let scene = manifest.reviewed_scenes[0].clone();
        let scene_end = scene.timeline_start_us.0 + scene.timeline_duration_us.0;

        let placed = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetSoundLayer {
                layer: room_tone_layer(&scene.id, scene.timeline_start_us.0, scene_end),
            }]),
        )
        .unwrap();
        assert_eq!(placed.manifest.sound_layers.len(), 1);

        let removed = apply_timeline_edit(
            &placed.manifest,
            &request(vec![VideoTimelineOperation::RemoveSoundLayer {
                layer_id: "tone-one".into(),
            }]),
        )
        .unwrap();
        assert!(removed.manifest.sound_layers.is_empty());
    }

    #[test]
    fn registering_a_sound_labels_imported_media_and_removing_it_takes_its_placements() {
        let mut manifest = manifest();
        manifest.source_assets.push(music_asset());
        manifest.validate_strict().unwrap();
        let scene = manifest.reviewed_scenes[0].clone();
        let scene_end = scene.timeline_start_us.0 + scene.timeline_duration_us.0;

        let registered = apply_timeline_edit(
            &manifest,
            &request(vec![
                VideoTimelineOperation::RegisterSoundAsset {
                    asset_id: "sound-tone".into(),
                    source_asset_id: "music-one".into(),
                    name: "Quiet room".into(),
                    tags: vec!["room tone".into()],
                },
                VideoTimelineOperation::SetSoundLayer {
                    layer: room_tone_layer(&scene.id, scene.timeline_start_us.0, scene_end),
                },
            ]),
        )
        .unwrap();
        assert_eq!(registered.manifest.sound_assets.len(), 1);
        assert_eq!(registered.manifest.sound_layers.len(), 1);

        // Removing the sound removes its uses rather than leaving a placement with no audio.
        let removed = apply_timeline_edit(
            &registered.manifest,
            &request(vec![VideoTimelineOperation::RemoveSoundAsset {
                asset_id: "sound-tone".into(),
            }]),
        )
        .unwrap();
        assert!(removed.manifest.sound_assets.is_empty());
        assert!(removed.manifest.sound_layers.is_empty());
    }

    #[test]
    fn sound_design_can_only_label_media_the_project_already_imported() {
        let manifest = manifest();
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::RegisterSoundAsset {
                asset_id: "sound-tone".into(),
                source_asset_id: "source-absent".into(),
                name: "Quiet room".into(),
                tags: vec![],
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);

        // Media with no audio track cannot become sound design, however it was imported.
        let mut silent = manifest.clone();
        let mut silent_source = music_asset();
        silent_source.id = "silent-clip".into();
        // A silent video: it has picture and no audio, so it can never be sound design.
        silent_source.probe.has_audio = false;
        silent_source.probe.has_video = true;
        silent_source.probe.width = Some(1920);
        silent_source.probe.height = Some(1080);
        silent_source.probe.frame_rate = Some(RationalFrameRate::FPS_30);
        silent.source_assets.push(silent_source);
        silent.validate_strict().unwrap();
        let error = apply_timeline_edit(
            &silent,
            &request(vec![VideoTimelineOperation::RegisterSoundAsset {
                asset_id: "sound-tone".into(),
                source_asset_id: "silent-clip".into(),
                name: "Quiet room".into(),
                tags: vec![],
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidAsset);
    }

    #[test]
    fn one_managed_source_is_registered_as_one_sound() {
        let mut manifest = manifest();
        manifest.source_assets.push(music_asset());
        let first = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::RegisterSoundAsset {
                asset_id: "sound-tone".into(),
                source_asset_id: "music-one".into(),
                name: "Quiet room".into(),
                tags: vec![],
            }]),
        )
        .unwrap();
        let error = apply_timeline_edit(
            &first.manifest,
            &request(vec![VideoTimelineOperation::RegisterSoundAsset {
                asset_id: "sound-other".into(),
                source_asset_id: "music-one".into(),
                name: "Same audio, different label".into(),
                tags: vec![],
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DuplicateId);
    }

    #[test]
    fn a_placement_cannot_invent_a_sound_the_project_does_not_have() {
        let manifest = manifest();
        let scene = manifest.reviewed_scenes[0].clone();
        let scene_end = scene.timeline_start_us.0 + scene.timeline_duration_us.0;
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetSoundLayer {
                layer: room_tone_layer(&scene.id, scene.timeline_start_us.0, scene_end),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn room_tone_that_stops_early_is_rejected_by_the_manifest() {
        let mut manifest = manifest();
        manifest.source_assets.push(music_asset());
        manifest.sound_assets.push(sound_asset());
        let scene = manifest.reviewed_scenes[0].clone();
        let short_end = scene.timeline_start_us.0 + scene.timeline_duration_us.0 / 2;
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetSoundLayer {
                layer: room_tone_layer(&scene.id, scene.timeline_start_us.0, short_end),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidSoundPlacement);
    }

    #[test]
    fn removing_a_placement_that_does_not_exist_is_rejected() {
        let manifest = manifest();
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::RemoveSoundLayer {
                layer_id: "tone-absent".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn promoting_a_draft_line_re_reads_only_that_line() {
        let mut manifest = with_takes(&with_dialogue(&manifest()));
        // Hear the whole episode with stand-ins first.
        for binding in &mut manifest.narration_bindings {
            binding.fidelity = TakeFidelity::Draft;
        }
        manifest.validate_strict().unwrap();
        assert_eq!(manifest.draft_turn_ids().len(), 2);

        let promoted = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PromoteTurnsToFinal {
                turn_ids: vec!["turn-b".into()],
            }]),
        )
        .unwrap();

        // Only the promoted line loses its take, so only it is re-read.
        let surviving = promoted
            .manifest
            .narration_bindings
            .iter()
            .map(|binding| binding.turn_id.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(surviving, vec!["turn-a".to_string()]);
        assert!(promoted
            .receipt
            .invalidated_stages
            .contains(&RevisionStage::Speech));
    }

    #[test]
    fn a_line_that_is_already_final_cannot_be_promoted() {
        let manifest = with_takes(&with_dialogue(&manifest()));
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::PromoteTurnsToFinal {
                turn_ids: vec!["turn-a".into()],
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);
    }

    #[test]
    fn a_master_cannot_be_published_while_a_line_is_still_a_stand_in() {
        use super::super::contracts::{PublicationState, RenderArtifact, RenderArtifactRole};

        let mut manifest = with_takes(&with_dialogue(&manifest()));
        manifest.render_artifacts.push(RenderArtifact {
            id: "master".into(),
            role: RenderArtifactRole::FinalMaster,
            scene_id: None,
            managed_path: "renders/master.mp4".into(),
            sha256: "9".repeat(64),
            cache_key: "8".repeat(64),
            mime_type: "video/mp4".into(),
            duration_us: Some(Microseconds(3_000_000)),
            width: Some(1080),
            height: Some(1920),
            publication_state: PublicationState::Published,
            created_at: NOW.into(),
        });
        manifest.validate_strict().unwrap();

        manifest.narration_bindings[0].fidelity = TakeFidelity::Draft;
        let error = manifest.validate_strict().unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DraftNotPromoted);
    }

    #[test]
    fn a_beat_edit_must_name_a_turn_that_exists() {
        let manifest = with_dialogue(&manifest());
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetTurnBeat {
                turn_id: "turn-absent".into(),
                lead_in_us: Microseconds(500_000),
                overlap_us: Microseconds::ZERO,
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn clearing_a_beat_that_is_already_derived_is_rejected_as_no_change() {
        let manifest = with_dialogue(&manifest());
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::ClearTurnBeat {
                turn_id: "turn-b".into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);
    }

    #[test]
    fn a_beat_cannot_both_hold_and_overlap() {
        let manifest = with_dialogue(&manifest());
        let error = apply_timeline_edit(
            &manifest,
            &request(vec![VideoTimelineOperation::SetTurnBeat {
                turn_id: "turn-b".into(),
                lead_in_us: Microseconds(500_000),
                overlap_us: Microseconds(500_000),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidPerformance);
    }

    fn request(operations: Vec<VideoTimelineOperation>) -> VideoTimelineEditRequest {
        VideoTimelineEditRequest {
            project_id: "project-editor".into(),
            expected_revision: 0,
            base_version_id: "store-version-1".into(),
            operation_id: "timeline-operation-1".into(),
            operations,
        }
    }

    fn add_narration_dependency(manifest: &mut VideoProjectManifest, scene_id: &str, suffix: &str) {
        let artifact_id = format!("artifact-narration-{suffix}");
        manifest.render_artifacts.push(RenderArtifact {
            id: artifact_id.clone(),
            role: RenderArtifactRole::SceneSegment,
            scene_id: None,
            managed_path: format!("renders/narration-{suffix}.wav"),
            sha256: "e".repeat(64),
            cache_key: "f".repeat(64),
            mime_type: "audio/wav".into(),
            duration_us: Some(Microseconds(3_000_000)),
            width: None,
            height: None,
            publication_state: PublicationState::Published,
            created_at: NOW.into(),
        });
        let script = manifest
            .reviewed_scenes
            .iter()
            .find(|scene| scene.id == scene_id)
            .unwrap()
            .script
            .clone();
        manifest.narration_bindings.push(NarrationBinding {
            id: format!("narration-{suffix}"),
            scene_id: Some(scene_id.into()),
            turn_id: None,
            character_id: None,
            lexicon_fingerprint: None,
            fidelity: TakeFidelity::Final,
            render_artifact_id: artifact_id,
            history_id: format!("history-{suffix}"),
            generation_job_id: format!("job-{suffix}"),
            voice_id: format!("voice-{suffix}"),
            model_id: "model-test".into(),
            speaker: "speaker".into(),
            language: "en".into(),
            script_sha256: format!("{:x}", Sha256::digest(script.as_bytes())),
            created_at: NOW.into(),
        });
        manifest.validate_strict().unwrap();
    }

    fn split_operation(at_timeline_us: i64) -> VideoTimelineOperation {
        VideoTimelineOperation::SplitScene {
            scene_id: FIRST_SCENE_ID.into(),
            at_timeline_us: Microseconds(at_timeline_us),
        }
    }

    #[test]
    fn structured_requests_reject_unknown_fields() {
        let top_level = json!({
            "project_id": "project-editor",
            "expected_revision": 0,
            "base_version_id": "store-version-1",
            "operation_id": "timeline-operation-1",
            "operations": [{"type": "reorder_scene", "scene_id": FIRST_SCENE_ID, "to_index": 1}],
            "surprise": true,
        });
        assert!(serde_json::from_value::<VideoTimelineEditRequest>(top_level).is_err());
        let operation = json!({
            "type": "split_scene",
            "scene_id": FIRST_SCENE_ID,
            "at_timeline_us": 1_500_000,
            "milliseconds": 1_500,
        });
        assert!(serde_json::from_value::<VideoTimelineOperation>(operation).is_err());
    }

    #[test]
    fn split_uses_exact_source_clock_and_partitions_tracks_gaps_captions_and_layout() {
        let original = manifest();
        let applied =
            apply_timeline_edit(&original, &request(vec![split_operation(1_500_000)])).unwrap();
        let right_id = deterministic_split_id("scene", FIRST_SCENE_ID, Microseconds(1_500_000));
        assert_eq!(applied.manifest.reviewed_scenes.len(), 3);
        assert_eq!(
            applied.manifest.reviewed_scenes[0].source_range,
            Some(range(1_000_000, 2_500_000))
        );
        assert_eq!(applied.manifest.reviewed_scenes[1].id, right_id);
        assert_eq!(
            applied.manifest.reviewed_scenes[1].source_range,
            Some(range(2_500_000, 4_000_000))
        );
        let first_video = &applied.manifest.tracks[0].clips[0];
        let right_video = &applied.manifest.tracks[0].clips[1];
        assert_eq!(first_video.source_range, range(1_000_000, 2_500_000));
        assert_eq!(right_video.source_range, range(2_500_000, 4_000_000));
        assert_eq!(right_video.scene_id.as_deref(), Some(right_id.as_str()));
        let split_overlay = applied
            .manifest
            .gaps
            .iter()
            .filter(|gap| gap.track_id == "overlay-empty" && gap.range.end_us.0 <= 3_000_000)
            .collect::<Vec<_>>();
        assert_eq!(split_overlay.len(), 2);
        assert_eq!(split_overlay[0].range, range(0, 1_500_000));
        assert_eq!(split_overlay[1].range, range(1_500_000, 3_000_000));
        let first_caption = &applied.manifest.captions[0];
        let right_caption = &applied.manifest.captions[1];
        assert_eq!(first_caption.range, range(100_000, 1_500_000));
        assert_eq!(right_caption.range, range(1_500_000, 2_900_000));
        assert_eq!(first_caption.text, "one two ");
        assert_eq!(right_caption.text, "three four");
        assert_eq!(
            format!("{}{}", first_caption.text, right_caption.text),
            "one two three four"
        );
        assert_eq!(right_caption.scene_id.as_deref(), Some(right_id.as_str()));
        assert!(applied
            .manifest
            .layout
            .elements
            .iter()
            .any(|element| element.scene_id.as_deref() == Some(right_id.as_str())));
        assert_eq!(applied.manifest.revision, original.revision);
        assert_eq!(applied.manifest.revision_history, original.revision_history);
        assert_eq!(applied.manifest.updated_at, original.updated_at);
        assert_eq!(
            applied.receipt.changed_paths,
            vec![
                "/captions",
                "/gaps",
                "/layout/elements",
                "/reviewed_scenes",
                "/tracks",
            ]
        );
        assert!(applied
            .receipt
            .invalidated_stages
            .contains(&RevisionStage::Plan));
        assert!(applied
            .receipt
            .invalidated_stages
            .contains(&RevisionStage::PublishPackage));
        applied.manifest.validate_strict().unwrap();
    }

    #[test]
    fn split_enforces_minimums_and_rejects_inexact_rational_boundaries() {
        let original = manifest();
        let error =
            apply_timeline_edit(&original, &request(vec![split_operation(99_999)])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);
        apply_timeline_edit(&original, &request(vec![split_operation(100_000)])).unwrap();

        let mut rational = manifest();
        rational.reviewed_scenes[0].source_range = Some(range(1_000_000, 5_500_000));
        rational.candidates[0].source_range = range(1_000_000, 5_500_000);
        for clip in rational
            .tracks
            .iter_mut()
            .flat_map(|track| &mut track.clips)
            .filter(|clip| clip.scene_id.as_deref() == Some(FIRST_SCENE_ID))
        {
            clip.source_range = range(1_000_000, 5_500_000);
            clip.playback_rate = RationalRate {
                numerator: 3,
                denominator: 2,
            };
        }
        rational.validate_strict().unwrap();
        let error =
            apply_timeline_edit(&rational, &request(vec![split_operation(1_500_001)])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DurationMismatch);
    }

    #[test]
    fn trim_clamps_source_derived_caption_text_and_preserves_inter_scene_gaps() {
        let original = manifest();
        let applied = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::TrimScene {
                scene_id: FIRST_SCENE_ID.into(),
                source_start_us: Microseconds(1_500_000),
                source_end_us: Microseconds(3_500_000),
            }]),
        )
        .unwrap();
        assert_eq!(
            applied.manifest.timeline_duration_us,
            Microseconds(6_000_000)
        );
        assert_eq!(
            applied.manifest.reviewed_scenes[0].source_range,
            Some(range(1_500_000, 3_500_000))
        );
        assert_eq!(
            applied.manifest.reviewed_scenes[1].timeline_start_us,
            Microseconds(3_000_000)
        );
        assert_eq!(
            applied.manifest.tracks[0].clips[0].source_range,
            range(1_500_000, 3_500_000)
        );
        assert!(
            applied
                .manifest
                .gaps
                .iter()
                .filter(|gap| gap.range == range(2_000_000, 3_000_000))
                .count()
                >= 3
        );
        assert_eq!(applied.manifest.captions[0].range, range(0, 2_000_000));
        assert_eq!(applied.manifest.captions[0].text, "two three four");
        assert!(applied
            .receipt
            .changed_paths
            .contains(&"/timeline_duration_us".to_string()));
        applied.manifest.validate_strict().unwrap();
    }

    #[test]
    fn trim_drops_empty_caption_intersections_and_rejects_subminimum_ranges() {
        let original = manifest();
        let applied = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::TrimScene {
                scene_id: FIRST_SCENE_ID.into(),
                source_start_us: Microseconds(2_800_000),
                source_end_us: Microseconds(3_000_000),
            }]),
        )
        .unwrap();
        assert!(applied
            .manifest
            .captions
            .iter()
            .all(|caption| caption.scene_id.as_deref() != Some(FIRST_SCENE_ID)));
        let error = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::TrimScene {
                scene_id: FIRST_SCENE_ID.into(),
                source_start_us: Microseconds(2_800_000),
                source_end_us: Microseconds(2_899_999),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);
    }

    #[test]
    fn reorder_moves_scene_local_media_while_preserving_gap_slots_and_source_ranges() {
        let original = manifest();
        let applied = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::ReorderScene {
                scene_id: FIRST_SCENE_ID.into(),
                to_index: 1,
            }]),
        )
        .unwrap();
        assert_eq!(applied.manifest.reviewed_scenes[0].id, SECOND_SCENE_ID);
        assert_eq!(
            applied.manifest.reviewed_scenes[0].source_range,
            original.reviewed_scenes[1].source_range
        );
        assert_eq!(
            applied.manifest.reviewed_scenes[1].timeline_start_us,
            Microseconds(4_000_000)
        );
        assert!(
            applied
                .manifest
                .gaps
                .iter()
                .filter(|gap| gap.range == range(3_000_000, 4_000_000))
                .count()
                >= 3
        );
        let second_clip = applied.manifest.tracks[0]
            .clips
            .iter()
            .find(|clip| clip.scene_id.as_deref() == Some(SECOND_SCENE_ID))
            .unwrap();
        assert_eq!(second_clip.timeline_start_us, Microseconds::ZERO);
        assert_eq!(second_clip.source_range, range(8_000_000, 11_000_000));
        assert_eq!(
            applied.manifest.layout.elements[0].scene_id.as_deref(),
            Some(FIRST_SCENE_ID)
        );
        applied.manifest.validate_strict().unwrap();
    }

    #[test]
    fn visual_layers_follow_reorder_and_fail_closed_when_a_split_would_change_motion() {
        let mut original = manifest();
        add_visual_layer(&mut original, range(500_000, 1_000_000), FIRST_SCENE_ID);
        original.validate_strict().unwrap();
        let reordered = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::ReorderScene {
                scene_id: FIRST_SCENE_ID.into(),
                to_index: 1,
            }]),
        )
        .unwrap();
        assert_eq!(
            reordered.manifest.visual_layers[0].range,
            range(4_500_000, 5_000_000)
        );
        assert_eq!(
            reordered.manifest.visual_layers[0].scene_id.as_deref(),
            Some(FIRST_SCENE_ID)
        );
        assert!(reordered
            .receipt
            .changed_paths
            .iter()
            .any(|path| path == "/visual_layers"));

        let mut crossing = manifest();
        add_visual_layer(&mut crossing, range(500_000, 2_000_000), FIRST_SCENE_ID);
        crossing.validate_strict().unwrap();
        let error = apply_timeline_edit(&crossing, &request(vec![split_operation(1_500_000)]))
            .expect_err("a split cannot silently retime an active visual motion curve");
        assert_eq!(error.code, VideoErrorCode::InvalidLayout);
        assert_eq!(error.field.as_deref(), Some("visual_layers.range"));
    }

    #[test]
    fn visual_layer_motion_is_revisioned_without_replacing_the_managed_asset() {
        let mut original = manifest();
        add_visual_layer(&mut original, range(500_000, 2_500_000), FIRST_SCENE_ID);
        original.validate_strict().unwrap();
        let original_asset = original.visual_assets[0].clone();
        let motion = VisualMotion {
            start_bounds: NormalizedRect {
                x_bp: 2_000,
                y_bp: 3_000,
                width_bp: 6_400,
                height_bp: 3_600,
            },
            end_bounds: NormalizedRect {
                x_bp: 1_000,
                y_bp: 2_000,
                width_bp: 8_000,
                height_bp: 4_500,
            },
            start_opacity_milli: 900,
            end_opacity_milli: 900,
            start_rotation_milli_degrees: 0,
            end_rotation_milli_degrees: 0,
            easing: VisualEasing::Linear,
        };
        let update = VideoTimelineOperation::UpdateVisualLayer {
            layer_id: "visual-layer-editor".into(),
            scene_id: Some(FIRST_SCENE_ID.into()),
            range: range(750_000, 2_250_000),
            fit: VisualFit::Cover,
            crop: None,
            z_index: 7,
            motion: motion.clone(),
            transition_in_us: Microseconds(200_000),
            transition_out_us: Microseconds(150_000),
        };
        let applied = apply_timeline_edit(&original, &request(vec![update.clone()])).unwrap();
        assert_eq!(applied.manifest.visual_assets[0], original_asset);
        let layer = &applied.manifest.visual_layers[0];
        assert_eq!(layer.asset_id, "visual-editor");
        assert_eq!(layer.range, range(750_000, 2_250_000));
        assert_eq!(layer.fit, VisualFit::Cover);
        assert_eq!(layer.z_index, 7);
        assert_eq!(layer.motion, motion);
        assert_eq!(
            applied.receipt.changed_paths,
            vec!["/visual_layers".to_string()]
        );
        assert_eq!(applied.manifest.revision, original.revision);

        let mut outside_scene = update;
        if let VideoTimelineOperation::UpdateVisualLayer { range, .. } = &mut outside_scene {
            *range = TimeRange::new(2_500_000, 3_500_000).unwrap();
        }
        let error = apply_timeline_edit(&original, &request(vec![outside_scene])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidLayout);
        assert_eq!(error.field.as_deref(), Some("visual_layers.range"));
    }

    #[test]
    fn deterministic_split_merge_is_an_exact_manifest_round_trip() {
        let original = manifest();
        let split =
            apply_timeline_edit(&original, &request(vec![split_operation(1_500_000)])).unwrap();
        let right_id = split.manifest.reviewed_scenes[1].id.clone();
        let merged = apply_timeline_edit(
            &split.manifest,
            &request(vec![VideoTimelineOperation::MergeScenes {
                first_scene_id: FIRST_SCENE_ID.into(),
                second_scene_id: right_id,
            }]),
        )
        .unwrap();
        assert_eq!(merged.manifest, original);
        assert_eq!(merged.manifest.revision, split.manifest.revision);
    }

    #[test]
    fn merge_rejects_modified_or_arbitrary_siblings() {
        let original = manifest();
        let split =
            apply_timeline_edit(&original, &request(vec![split_operation(1_500_000)])).unwrap();
        let right_id = split.manifest.reviewed_scenes[1].id.clone();
        let mut incompatible = split.manifest;
        incompatible.tracks[0].clips[1].muted = true;
        incompatible.validate_strict().unwrap();
        let error = apply_timeline_edit(
            &incompatible,
            &request(vec![VideoTimelineOperation::MergeScenes {
                first_scene_id: FIRST_SCENE_ID.into(),
                second_scene_id: right_id,
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);

        let error = apply_timeline_edit(
            &original,
            &request(vec![VideoTimelineOperation::MergeScenes {
                first_scene_id: FIRST_SCENE_ID.into(),
                second_scene_id: SECOND_SCENE_ID.into(),
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);
    }

    #[test]
    fn split_fails_closed_for_rendered_scene_dependencies() {
        let mut rendered = manifest();
        rendered.render_artifacts.push(RenderArtifact {
            id: "artifact-scene-first".into(),
            role: RenderArtifactRole::SceneSegment,
            scene_id: Some(FIRST_SCENE_ID.into()),
            managed_path: "renders/scene-first.mp4".into(),
            sha256: "c".repeat(64),
            cache_key: "d".repeat(64),
            mime_type: "video/mp4".into(),
            duration_us: Some(Microseconds(3_000_000)),
            width: Some(1080),
            height: Some(1920),
            publication_state: PublicationState::Published,
            created_at: NOW.into(),
        });
        rendered.validate_strict().unwrap();
        let error =
            apply_timeline_edit(&rendered, &request(vec![split_operation(1_500_000)])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);
    }

    #[test]
    fn narration_dependencies_fail_closed_for_split_and_merge_but_survive_reorder() {
        let mut narrated = manifest();
        add_narration_dependency(&mut narrated, FIRST_SCENE_ID, "first");
        let error =
            apply_timeline_edit(&narrated, &request(vec![split_operation(1_500_000)])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);

        let reordered = apply_timeline_edit(
            &narrated,
            &request(vec![VideoTimelineOperation::ReorderScene {
                scene_id: FIRST_SCENE_ID.into(),
                to_index: 1,
            }]),
        )
        .unwrap();
        assert_eq!(
            reordered.manifest.narration_bindings[0].scene_id.as_deref(),
            Some(FIRST_SCENE_ID)
        );
        reordered.manifest.validate_strict().unwrap();

        let original = manifest();
        let split =
            apply_timeline_edit(&original, &request(vec![split_operation(1_500_000)])).unwrap();
        let right_id = split.manifest.reviewed_scenes[1].id.clone();
        let mut right_narrated = split.manifest;
        add_narration_dependency(&mut right_narrated, &right_id, "right");
        let error = apply_timeline_edit(
            &right_narrated,
            &request(vec![VideoTimelineOperation::MergeScenes {
                first_scene_id: FIRST_SCENE_ID.into(),
                second_scene_id: right_id,
            }]),
        )
        .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidScene);
    }

    #[test]
    fn deterministic_id_collisions_and_scene_revision_overflow_fail_closed() {
        let mut collision = manifest();
        let derived = deterministic_split_id("scene", FIRST_SCENE_ID, Microseconds(1_500_000));
        collision.reviewed_scenes[1].id = derived.clone();
        for clip in collision
            .tracks
            .iter_mut()
            .flat_map(|track| &mut track.clips)
            .filter(|clip| clip.scene_id.as_deref() == Some(SECOND_SCENE_ID))
        {
            clip.scene_id = Some(derived.clone());
        }
        for caption in &mut collision.captions {
            if caption.scene_id.as_deref() == Some(SECOND_SCENE_ID) {
                caption.scene_id = Some(derived.clone());
            }
        }
        collision.validate_strict().unwrap();
        let error = apply_timeline_edit(&collision, &request(vec![split_operation(1_500_000)]))
            .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DuplicateId);

        let mut overflow = manifest();
        overflow.reviewed_scenes[0].revision = u32::MAX;
        overflow.validate_strict().unwrap();
        let error =
            apply_timeline_edit(&overflow, &request(vec![split_operation(1_500_000)])).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::ArithmeticOverflow);
    }

    #[test]
    fn stale_project_or_revision_is_rejected_but_opaque_store_version_is_echoed() {
        let original = manifest();
        let mut stale_project = request(vec![split_operation(1_500_000)]);
        stale_project.project_id = "project-other".into();
        assert_eq!(
            apply_timeline_edit(&original, &stale_project)
                .unwrap_err()
                .code,
            VideoErrorCode::MissingReference
        );
        let mut stale_revision = request(vec![split_operation(1_500_000)]);
        stale_revision.expected_revision = 1;
        assert_eq!(
            apply_timeline_edit(&original, &stale_revision)
                .unwrap_err()
                .code,
            VideoErrorCode::InvalidRevision
        );
        let mut current = request(vec![split_operation(1_500_000)]);
        current.base_version_id = "opaque-store-version".into();
        let applied = apply_timeline_edit(&original, &current).unwrap();
        assert_eq!(applied.receipt.base_version_id, "opaque-store-version");
    }

    #[test]
    fn deterministic_replay_and_ordered_batch_do_not_advance_revision() {
        let original = manifest();
        let replay_request = request(vec![split_operation(1_500_000)]);
        assert_eq!(
            apply_timeline_edit(&original, &replay_request).unwrap(),
            apply_timeline_edit(&original, &replay_request).unwrap()
        );

        let batch = request(vec![
            VideoTimelineOperation::TrimScene {
                scene_id: FIRST_SCENE_ID.into(),
                source_start_us: Microseconds(1_500_000),
                source_end_us: Microseconds(3_500_000),
            },
            VideoTimelineOperation::ReorderScene {
                scene_id: SECOND_SCENE_ID.into(),
                to_index: 0,
            },
        ]);
        let applied = apply_timeline_edit(&original, &batch).unwrap();
        assert_eq!(applied.manifest.revision, original.revision);
        assert_eq!(applied.receipt.operation_id, batch.operation_id);
        assert_eq!(applied.receipt.expected_revision, original.revision);
        applied.manifest.validate_strict().unwrap();
    }
}
