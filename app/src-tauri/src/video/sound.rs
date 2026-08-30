//! Sound design: registered local audio placed under and around the dialogue.
//!
//! After voices, sound design is the largest single difference between an episode that sounds
//! produced and one that sounds like takes glued together. Continuous room tone under a scene does
//! most of that work on its own, because it removes the digital silence between takes that tells a
//! listener each line was recorded separately.
//!
//! Nothing here generates audio. Assets are files the user already has and registers, carrying the
//! same rights and provenance record as any other imported media. That keeps the whole feature
//! outside the licensing and hardware-qualification questions a generative sound-effect model would
//! raise, while delivering the part that actually changes how an episode sounds.

use super::contracts::{
    validate_identifier, validate_managed_path, validate_nonempty, validate_sha256,
    validate_timestamp_text, Microseconds, Provenance, TimeRange, Validate, VideoError,
    VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SOUND_ASSETS: usize = 256;
pub const MAX_SOUND_LAYERS: usize = 512;
pub const MAX_SOUND_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_SOUND_DURATION_US: i64 = 60 * 60 * 1_000_000;
pub const MAX_TAGS_PER_ASSET: usize = 16;
pub const MAX_TAG_BYTES: usize = 48;

/// Room tone sits far under everything else. A level anywhere near the dialogue stops reading as
/// the sound of a room and starts reading as noise.
pub const MAX_ROOM_TONE_GAIN_DB_MILLI: i32 = -18_000;

/// Container formats soundAr's local pipeline decodes for sound design.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundMimeType {
    Wav,
    Flac,
    Ogg,
    Mp3,
}

impl SoundMimeType {
    pub const fn as_mime(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Flac => "audio/flac",
            Self::Ogg => "audio/ogg",
            Self::Mp3 => "audio/mpeg",
        }
    }
}

/// One registered sound-design file.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoundAsset {
    pub id: String,
    pub name: String,
    pub managed_path: String,
    pub sha256: String,
    pub mime_type: SoundMimeType,
    pub duration_us: Microseconds,
    pub sample_rate: u32,
    pub channels: u8,
    pub size_bytes: u64,
    /// Searchable labels - `rain`, `door`, `market`, `room-tone` - so a placement can be found by
    /// what it sounds like rather than by filename.
    #[serde(default)]
    pub tags: Vec<String>,
    pub provenance: Provenance,
    pub created_at: String,
}

impl Validate for SoundAsset {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "sound_assets.id")?;
        validate_nonempty(&self.name, "sound_assets.name", 256)?;
        validate_managed_path(&self.managed_path, "sound_assets.managed_path")?;
        validate_sha256(
            &self.sha256,
            "sound_assets.sha256",
            VideoErrorCode::InvalidAsset,
        )?;
        if !(1..=MAX_SOUND_DURATION_US).contains(&self.duration_us.0) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "a sound asset must be positive and no longer than one hour",
            )
            .at("sound_assets.duration_us"));
        }
        if !(8_000..=192_000).contains(&self.sample_rate) || !(1..=8).contains(&self.channels) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "sound asset sample rate or channel count is outside the supported envelope",
            )
            .at("sound_assets.sample_rate"));
        }
        if !(16..=MAX_SOUND_ASSET_BYTES).contains(&self.size_bytes) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "sound asset size is outside the supported envelope",
            )
            .at("sound_assets.size_bytes"));
        }
        if self.tags.len() > MAX_TAGS_PER_ASSET {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                format!("a sound asset supports at most {MAX_TAGS_PER_ASSET} tags"),
            )
            .at("sound_assets.tags"));
        }
        let mut seen = BTreeSet::new();
        for tag in &self.tags {
            let normalized = normalize_tag(tag);
            if normalized.is_empty() || tag.len() > MAX_TAG_BYTES {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    format!("a tag must be non-empty and at most {MAX_TAG_BYTES} bytes"),
                )
                .at("sound_assets.tags"));
            }
            if !seen.insert(normalized) {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidAsset,
                    "a sound asset lists the same tag twice",
                )
                .at("sound_assets.tags"));
            }
        }
        validate_timestamp_text(
            &self.provenance.imported_at,
            "sound_assets.provenance.imported_at",
        )?;
        validate_nonempty(
            &self.provenance.producer,
            "sound_assets.provenance.producer",
            256,
        )?;
        validate_timestamp_text(&self.created_at, "sound_assets.created_at")?;
        Ok(())
    }
}

/// What a placement is doing, which decides how it may be positioned and mixed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPlacementKind {
    /// A single event: a door, a match, a phone. Plays once at a point.
    OneShot,
    /// A place: rain, a market, traffic. Runs across a span and may loop to fill it.
    Ambience,
    /// The sound of the room itself, under an entire scene. This is what removes the digital
    /// silence between takes that makes an episode sound assembled.
    RoomTone,
}

impl SoundPlacementKind {
    /// Whether this placement may repeat its asset to cover a span longer than the asset itself.
    pub const fn may_loop(self) -> bool {
        matches!(self, Self::Ambience | Self::RoomTone)
    }
}

/// One placed sound.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoundLayer {
    pub id: String,
    pub asset_id: String,
    pub kind: SoundPlacementKind,
    /// Room tone and ambience belong to a scene; a one-shot may instead be anchored to the turn it
    /// punctuates so it moves when that line does.
    #[serde(default)]
    pub scene_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    pub range: TimeRange,
    pub gain_db_milli: i32,
    pub fade_in_us: Microseconds,
    pub fade_out_us: Microseconds,
    /// Repeat the asset to cover the whole range. Rejected for a one-shot, whose whole nature is
    /// happening once.
    #[serde(default)]
    pub loop_to_fill: bool,
    /// Reduce this layer while a voice is speaking over it.
    #[serde(default)]
    pub duck_under_speech: bool,
}

impl Validate for SoundLayer {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "sound_layers.id")?;
        validate_identifier(&self.asset_id, "sound_layers.asset_id")?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "sound_layers.scene_id")?;
        }
        if let Some(turn_id) = &self.turn_id {
            validate_identifier(turn_id, "sound_layers.turn_id")?;
        }
        self.range.validate()?;
        if !(-60_000..=12_000).contains(&self.gain_db_milli) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "sound layer gain is outside the supported range",
            )
            .at("sound_layers.gain_db_milli"));
        }
        if self.fade_in_us.0 < 0 || self.fade_out_us.0 < 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "sound layer fades cannot be negative",
            )
            .at("sound_layers.fade_in_us"));
        }
        let span = self.range.end_us.0.saturating_sub(self.range.start_us.0);
        if self
            .fade_in_us
            .0
            .checked_add(self.fade_out_us.0)
            .is_none_or(|total| total > span)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "sound layer fades cannot together exceed the placement",
            )
            .at("sound_layers.fade_out_us"));
        }
        if self.loop_to_fill && !self.kind.may_loop() {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "a one-shot happens once and cannot loop",
            )
            .at("sound_layers.loop_to_fill"));
        }
        match self.kind {
            SoundPlacementKind::OneShot => {
                // A one-shot with no anchor is a sound at a bare timestamp, which any later edit
                // silently moves away from the moment it was meant to punctuate.
                if self.scene_id.is_none() && self.turn_id.is_none() {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidSoundPlacement,
                        "a one-shot must be anchored to the scene or turn it punctuates",
                    )
                    .at("sound_layers.turn_id"));
                }
            }
            SoundPlacementKind::Ambience | SoundPlacementKind::RoomTone => {
                if self.scene_id.is_none() {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidSoundPlacement,
                        "ambience and room tone belong to a scene",
                    )
                    .at("sound_layers.scene_id"));
                }
                if self.turn_id.is_some() {
                    return Err(VideoError::new(
                        VideoErrorCode::InvalidSoundPlacement,
                        "ambience and room tone run under a whole scene, not one turn",
                    )
                    .at("sound_layers.turn_id"));
                }
            }
        }
        if matches!(self.kind, SoundPlacementKind::RoomTone)
            && self.gain_db_milli > MAX_ROOM_TONE_GAIN_DB_MILLI
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "room tone must sit far under the dialogue; at this level it reads as noise",
            )
            .at("sound_layers.gain_db_milli"));
        }
        Ok(())
    }
}

/// Find registered sound by what it sounds like.
///
/// Matching is on normalized tags so `Room Tone` and `room-tone` are the same label, which matters
/// because the assistant proposes placements from written stage directions rather than from a
/// controlled vocabulary.
pub fn assets_matching_tag<'a>(assets: &'a [SoundAsset], tag: &str) -> Vec<&'a SoundAsset> {
    let wanted = normalize_tag(tag);
    if wanted.is_empty() {
        return Vec::new();
    }
    assets
        .iter()
        .filter(|asset| asset.tags.iter().any(|tag| normalize_tag(tag) == wanted))
        .collect()
}

pub(crate) fn normalize_tag(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|character| if character.is_alphanumeric() { character } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Validate a project's sound design against the scenes, turns, and assets it references.
pub(crate) fn validate_sound_design(
    assets: &[SoundAsset],
    layers: &[SoundLayer],
    scene_ranges: &BTreeMap<&str, TimeRange>,
    turn_ids: &BTreeSet<&str>,
) -> VideoResult<()> {
    if assets.len() > MAX_SOUND_ASSETS {
        return Err(VideoError::new(
            VideoErrorCode::InvalidAsset,
            format!("a project supports at most {MAX_SOUND_ASSETS} sound assets"),
        )
        .at("sound_assets"));
    }
    if layers.len() > MAX_SOUND_LAYERS {
        return Err(VideoError::new(
            VideoErrorCode::InvalidSoundPlacement,
            format!("a project supports at most {MAX_SOUND_LAYERS} sound placements"),
        )
        .at("sound_layers"));
    }

    let mut asset_ids = BTreeSet::new();
    for asset in assets {
        asset.validate()?;
        if !asset_ids.insert(asset.id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::DuplicateId,
                format!("duplicate identifier {}", asset.id),
            )
            .at("sound_assets.id"));
        }
    }

    let mut layer_ids = BTreeSet::new();
    let mut room_tone_scenes = BTreeSet::new();
    for layer in layers {
        layer.validate()?;
        if !layer_ids.insert(layer.id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::DuplicateId,
                format!("duplicate identifier {}", layer.id),
            )
            .at("sound_layers.id"));
        }
        if !asset_ids.contains(layer.asset_id.as_str()) {
            return Err(VideoError::new(
                VideoErrorCode::MissingReference,
                "a sound placement references an unregistered asset",
            )
            .at("sound_layers.asset_id"));
        }
        if let Some(turn_id) = layer.turn_id.as_deref() {
            if !turn_ids.contains(turn_id) {
                return Err(VideoError::new(
                    VideoErrorCode::MissingReference,
                    "a sound placement is anchored to a dialogue turn that does not exist",
                )
                .at("sound_layers.turn_id"));
            }
        }
        let Some(scene_id) = layer.scene_id.as_deref() else {
            continue;
        };
        let scene_range = scene_ranges.get(scene_id).ok_or_else(|| {
            VideoError::new(
                VideoErrorCode::MissingReference,
                "a sound placement is anchored to a scene that does not exist",
            )
            .at("sound_layers.scene_id")
        })?;
        // A placement that leaves its own scene would keep playing over the cut into the next one.
        if layer.range.start_us < scene_range.start_us || layer.range.end_us > scene_range.end_us {
            return Err(VideoError::new(
                VideoErrorCode::InvalidSoundPlacement,
                "a sound placement must stay inside the scene it belongs to",
            )
            .at("sound_layers.range"));
        }
        if matches!(layer.kind, SoundPlacementKind::RoomTone) {
            // Room tone is the sound of one room. Two of them in one scene is two rooms.
            if !room_tone_scenes.insert(scene_id) {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidSoundPlacement,
                    "a scene has only one room, and so only one room tone",
                )
                .at("sound_layers.scene_id"));
            }
            // Room tone that stops partway through leaves the silence it exists to remove.
            if layer.range != *scene_range {
                return Err(VideoError::new(
                    VideoErrorCode::InvalidSoundPlacement,
                    "room tone must run under the whole scene",
                )
                .at("sound_layers.range"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::contracts::{Provenance, ProvenanceKind};

    fn provenance() -> Provenance {
        Provenance {
            kind: ProvenanceKind::UserUpload,
            original_uri: None,
            imported_at: "2026-01-01T00:00:00Z".into(),
            producer: "sound-test".into(),
            producer_version: None,
            metadata: BTreeMap::new(),
        }
    }

    fn asset(id: &str, tags: &[&str]) -> SoundAsset {
        SoundAsset {
            id: id.into(),
            name: "Rain on a tin roof".into(),
            managed_path: format!("sounds/{id}.wav"),
            sha256: "a".repeat(64),
            mime_type: SoundMimeType::Wav,
            duration_us: Microseconds(8_000_000),
            sample_rate: 48_000,
            channels: 2,
            size_bytes: 1_536_000,
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            provenance: provenance(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn layer(id: &str, kind: SoundPlacementKind, start: i64, end: i64) -> SoundLayer {
        SoundLayer {
            id: id.into(),
            asset_id: "sound-rain".into(),
            kind,
            scene_id: Some("scene-one".into()),
            turn_id: None,
            range: TimeRange::new(start, end).unwrap(),
            gain_db_milli: -24_000,
            fade_in_us: Microseconds(250_000),
            fade_out_us: Microseconds(250_000),
            loop_to_fill: false,
            duck_under_speech: false,
        }
    }

    fn scene_ranges() -> BTreeMap<&'static str, TimeRange> {
        BTreeMap::from([("scene-one", TimeRange::new(0, 30_000_000).unwrap())])
    }

    fn check(layers: &[SoundLayer]) -> VideoResult<()> {
        validate_sound_design(
            &[asset("sound-rain", &["rain"])],
            layers,
            &scene_ranges(),
            &BTreeSet::from(["turn-a"]),
        )
    }

    #[test]
    fn room_tone_runs_under_the_whole_scene() {
        let mut tone = layer("tone", SoundPlacementKind::RoomTone, 0, 30_000_000);
        tone.loop_to_fill = true;
        check(std::slice::from_ref(&tone)).unwrap();

        // Stopping partway leaves exactly the silence room tone exists to remove.
        let mut partial = tone.clone();
        partial.range = TimeRange::new(0, 20_000_000).unwrap();
        let error = check(&[partial]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidSoundPlacement);
    }

    #[test]
    fn a_scene_has_only_one_room_tone() {
        let first = layer("tone-a", SoundPlacementKind::RoomTone, 0, 30_000_000);
        let mut second = first.clone();
        second.id = "tone-b".into();
        let error = check(&[first, second]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidSoundPlacement);
    }

    #[test]
    fn room_tone_must_sit_far_under_the_dialogue() {
        let mut loud = layer("tone", SoundPlacementKind::RoomTone, 0, 30_000_000);
        loud.gain_db_milli = -6_000;
        assert_eq!(
            loud.validate().unwrap_err().code,
            VideoErrorCode::InvalidSoundPlacement
        );
    }

    #[test]
    fn a_one_shot_must_be_anchored_to_what_it_punctuates() {
        let mut floating = layer("door", SoundPlacementKind::OneShot, 1_000_000, 2_000_000);
        floating.scene_id = None;
        assert_eq!(
            floating.validate().unwrap_err().code,
            VideoErrorCode::InvalidSoundPlacement
        );

        let mut at_turn = floating.clone();
        at_turn.turn_id = Some("turn-a".into());
        at_turn.validate().unwrap();
    }

    #[test]
    fn a_one_shot_happens_once_and_cannot_loop() {
        let mut repeated = layer("door", SoundPlacementKind::OneShot, 1_000_000, 2_000_000);
        repeated.loop_to_fill = true;
        assert_eq!(
            repeated.validate().unwrap_err().code,
            VideoErrorCode::InvalidSoundPlacement
        );
        assert!(!SoundPlacementKind::OneShot.may_loop());
        assert!(SoundPlacementKind::Ambience.may_loop());
        assert!(SoundPlacementKind::RoomTone.may_loop());
    }

    #[test]
    fn ambience_belongs_to_a_scene_not_to_one_line() {
        let mut on_turn = layer("rain", SoundPlacementKind::Ambience, 0, 10_000_000);
        on_turn.turn_id = Some("turn-a".into());
        assert_eq!(
            on_turn.validate().unwrap_err().code,
            VideoErrorCode::InvalidSoundPlacement
        );
    }

    #[test]
    fn a_placement_cannot_spill_out_of_its_scene() {
        let spilling = layer("rain", SoundPlacementKind::Ambience, 0, 40_000_000);
        let error = check(&[spilling]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidSoundPlacement);
    }

    #[test]
    fn a_placement_must_reference_a_registered_asset() {
        let mut stray = layer("rain", SoundPlacementKind::Ambience, 0, 10_000_000);
        stray.asset_id = "sound-absent".into();
        let error = check(&[stray]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn a_placement_cannot_anchor_to_a_turn_that_does_not_exist() {
        let mut stray = layer("door", SoundPlacementKind::OneShot, 1_000_000, 2_000_000);
        stray.turn_id = Some("turn-absent".into());
        let error = check(&[stray]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::MissingReference);
    }

    #[test]
    fn fades_cannot_together_exceed_the_placement() {
        let mut greedy = layer("rain", SoundPlacementKind::Ambience, 0, 1_000_000);
        greedy.fade_in_us = Microseconds(800_000);
        greedy.fade_out_us = Microseconds(800_000);
        assert_eq!(
            greedy.validate().unwrap_err().code,
            VideoErrorCode::InvalidSoundPlacement
        );
    }

    #[test]
    fn sound_is_found_by_what_it_sounds_like_not_by_filename() {
        let assets = vec![
            asset("sound-rain", &["Rain", "Weather"]),
            asset("sound-tone", &["room tone"]),
        ];
        assert_eq!(assets_matching_tag(&assets, "rain").len(), 1);
        // The assistant proposes placements from written directions, so labels must match loosely.
        assert_eq!(assets_matching_tag(&assets, "Room-Tone").len(), 1);
        assert_eq!(assets_matching_tag(&assets, "  ROOM TONE  ").len(), 1);
        assert!(assets_matching_tag(&assets, "market").is_empty());
        assert!(assets_matching_tag(&assets, "   ").is_empty());
    }

    #[test]
    fn an_asset_cannot_list_the_same_tag_twice() {
        let duplicated = asset("sound-rain", &["rain", "Rain"]);
        assert_eq!(
            duplicated.validate().unwrap_err().code,
            VideoErrorCode::InvalidAsset
        );
    }

    #[test]
    fn duplicate_identifiers_are_rejected() {
        let first = layer("rain", SoundPlacementKind::Ambience, 0, 10_000_000);
        let second = first.clone();
        let error = check(&[first, second]).unwrap_err();
        assert_eq!(error.code, VideoErrorCode::DuplicateId);
    }
}
