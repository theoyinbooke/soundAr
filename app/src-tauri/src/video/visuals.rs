//! Canonical still-image and illustration composition contracts.
//!
//! Visual assets are timeless managed files. `VisualLayer` places one asset on the project clock
//! and carries deterministic start/end geometry, so a sequence of generated illustrations can be
//! animated locally without pretending each image is a video source.

use super::contracts::{
    validate_identifier, validate_managed_path, validate_nonempty, validate_sha256,
    validate_timestamp_text, Microseconds, NormalizedRect, Provenance, TimeRange, Validate,
    VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};

pub const MAX_VISUAL_ASSET_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_VISUAL_DIMENSION: u32 = 16_384;
pub const MAX_VISUAL_PIXELS: u64 = 100_000_000;
pub const MAX_VISUAL_ASSETS: usize = 64;
pub const MAX_VISUAL_LAYERS: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualMimeType {
    Png,
    Jpeg,
    Webp,
}

impl VisualMimeType {
    pub const fn as_mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualAsset {
    pub id: String,
    pub managed_path: String,
    pub sha256: String,
    pub mime_type: VisualMimeType,
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub size_bytes: u64,
    pub provenance: Provenance,
    pub created_at: String,
}

impl Validate for VisualAsset {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "visual_assets.id")?;
        validate_managed_path(&self.managed_path, "visual_assets.managed_path")?;
        validate_sha256(
            &self.sha256,
            "visual_assets.sha256",
            VideoErrorCode::InvalidAsset,
        )?;
        if self.width == 0
            || self.height == 0
            || self.width > MAX_VISUAL_DIMENSION
            || self.height > MAX_VISUAL_DIMENSION
            || u64::from(self.width) * u64::from(self.height) > MAX_VISUAL_PIXELS
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "visual dimensions exceed the supported image envelope",
            )
            .at("visual_assets.width"));
        }
        if !(16..=MAX_VISUAL_ASSET_BYTES).contains(&self.size_bytes) {
            return Err(VideoError::new(
                VideoErrorCode::InvalidAsset,
                "visual asset size is outside the supported envelope",
            )
            .at("visual_assets.size_bytes"));
        }
        validate_timestamp_text(&self.created_at, "visual_assets.created_at")?;
        validate_timestamp_text(
            &self.provenance.imported_at,
            "visual_assets.provenance.imported_at",
        )?;
        validate_nonempty(
            &self.provenance.producer,
            "visual_assets.provenance.producer",
            256,
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualFit {
    Cover,
    Contain,
    Stretch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualEasing {
    Linear,
    EaseInOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMotion {
    pub start_bounds: NormalizedRect,
    pub end_bounds: NormalizedRect,
    pub start_opacity_milli: u16,
    pub end_opacity_milli: u16,
    pub start_rotation_milli_degrees: i32,
    pub end_rotation_milli_degrees: i32,
    pub easing: VisualEasing,
}

impl Validate for VisualMotion {
    fn validate(&self) -> VideoResult<()> {
        self.start_bounds.validate()?;
        self.end_bounds.validate()?;
        if self.start_opacity_milli > 1_000 || self.end_opacity_milli > 1_000 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "visual opacity must be within 0..=1000",
            )
            .at("visual_layers.motion.opacity"));
        }
        if !(-360_000..=360_000).contains(&self.start_rotation_milli_degrees)
            || !(-360_000..=360_000).contains(&self.end_rotation_milli_degrees)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "visual rotation must be within one full turn",
            )
            .at("visual_layers.motion.rotation"));
        }
        if i64::from(self.start_bounds.width_bp) * i64::from(self.end_bounds.height_bp)
            != i64::from(self.end_bounds.width_bp) * i64::from(self.start_bounds.height_bp)
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "visual pan/zoom bounds must preserve one aspect ratio",
            )
            .at("visual_layers.motion.end_bounds"));
        }
        if self.start_opacity_milli != self.end_opacity_milli
            || self.start_rotation_milli_degrees != 0
            || self.end_rotation_milli_degrees != 0
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidLayout,
                "animated opacity and visual rotation are not enabled in this renderer version",
            )
            .at("visual_layers.motion"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualLayer {
    pub id: String,
    pub asset_id: String,
    pub scene_id: Option<String>,
    pub range: TimeRange,
    pub fit: VisualFit,
    pub crop: Option<NormalizedRect>,
    pub z_index: i16,
    pub motion: VisualMotion,
    pub transition_in_us: Microseconds,
    pub transition_out_us: Microseconds,
}

impl Validate for VisualLayer {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "visual_layers.id")?;
        validate_identifier(&self.asset_id, "visual_layers.asset_id")?;
        if let Some(scene_id) = &self.scene_id {
            validate_identifier(scene_id, "visual_layers.scene_id")?;
        }
        self.range.validate()?;
        if let Some(crop) = self.crop {
            crop.validate()?;
        }
        self.motion.validate()?;
        if self.transition_in_us.0 < 0 || self.transition_out_us.0 < 0 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidTimestamp,
                "visual transition durations may not be negative",
            )
            .at("visual_layers.transition"));
        }
        let transitions = self.transition_in_us.checked_add(self.transition_out_us)?;
        if transitions > self.range.duration()? {
            return Err(VideoError::new(
                VideoErrorCode::DurationMismatch,
                "visual transitions may not exceed the layer duration",
            )
            .at("visual_layers.transition"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{ProvenanceKind, TimeRange};
    use std::collections::BTreeMap;

    fn asset() -> VisualAsset {
        VisualAsset {
            id: "visual-one".into(),
            managed_path: "projects/project-one/visuals/visual-one.png".into(),
            sha256: "a".repeat(64),
            mime_type: VisualMimeType::Png,
            width: 1080,
            height: 1920,
            has_alpha: true,
            size_bytes: 1_024,
            provenance: Provenance {
                kind: ProvenanceKind::GeneratedLocally,
                original_uri: None,
                imported_at: "2026-08-28T12:00:00.000Z".into(),
                producer: "codex-image-tool".into(),
                producer_version: Some("1".into()),
                metadata: BTreeMap::from([
                    (
                        "prompt".into(),
                        serde_json::json!("A quiet illustrated studio"),
                    ),
                    (
                        "generation_job_id".into(),
                        serde_json::json!("image-job-one"),
                    ),
                ]),
            },
            created_at: "2026-08-28T12:00:00.000Z".into(),
        }
    }

    fn layer() -> VisualLayer {
        VisualLayer {
            id: "visual-layer-one".into(),
            asset_id: "visual-one".into(),
            scene_id: Some("scene-one".into()),
            range: TimeRange::new(0, 3_000_000).unwrap(),
            fit: VisualFit::Cover,
            crop: None,
            z_index: 2,
            motion: VisualMotion {
                start_bounds: NormalizedRect {
                    x_bp: 0,
                    y_bp: 0,
                    width_bp: 10_000,
                    height_bp: 10_000,
                },
                end_bounds: NormalizedRect {
                    x_bp: 200,
                    y_bp: 200,
                    width_bp: 9_600,
                    height_bp: 9_600,
                },
                start_opacity_milli: 1_000,
                end_opacity_milli: 1_000,
                start_rotation_milli_degrees: 0,
                end_rotation_milli_degrees: 0,
                easing: VisualEasing::EaseInOut,
            },
            transition_in_us: Microseconds(250_000),
            transition_out_us: Microseconds(250_000),
        }
    }

    #[test]
    fn generated_visual_assets_and_motion_are_strict_and_bounded() {
        asset().validate().expect("generated visual asset");
        layer().validate().expect("timed visual layer");

        let mut invalid = layer();
        invalid.motion.end_opacity_milli = 1_001;
        assert_eq!(
            invalid.validate().unwrap_err().stable_code(),
            "video.invalid_layout"
        );

        let mut invalid = layer();
        invalid.transition_in_us = Microseconds(2_000_000);
        invalid.transition_out_us = Microseconds(2_000_000);
        assert_eq!(
            invalid.validate().unwrap_err().stable_code(),
            "video.duration_mismatch"
        );
    }
}
