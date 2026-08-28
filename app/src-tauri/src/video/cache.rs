use super::contracts::{
    validate_identifier, validate_sha256, VideoError, VideoErrorCode, VideoResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

const CACHE_KEY_NAMESPACE: &[u8] = b"soundar-video-cache-v1\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStage {
    Probe,
    Proxy,
    Thumbnail,
    Waveform,
    Transcription,
    SceneAnalysis,
    Plan,
    Speech,
    Music,
    Image,
    Tracking,
    Captions,
    AudioMix,
    SceneRender,
    PreviewRender,
    FinalRender,
    PublishPackage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn parse(value: impl Into<String>) -> VideoResult<Self> {
        let value = value.into();
        validate_sha256(&value, "cache_key", VideoErrorCode::InvalidCacheInput)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CacheArtifactInput {
    pub role: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CacheKeyInput {
    pub stage: CacheStage,
    pub stage_version: String,
    pub scene_id: Option<String>,
    pub input_artifacts: Vec<CacheArtifactInput>,
    pub tool_versions: BTreeMap<String, String>,
    /// Only the manifest fields consumed by this stage, never the whole mutable manifest.
    pub manifest_slice: Value,
    pub profile: Value,
}

impl CacheKeyInput {
    pub fn key(&self) -> VideoResult<CacheKey> {
        validate_cache_input(self)?;
        let mut normalized = self.clone();
        normalized
            .input_artifacts
            .sort_by(|left, right| (&left.role, &left.sha256).cmp(&(&right.role, &right.sha256)));
        normalized.manifest_slice = canonicalize_value(&normalized.manifest_slice)?;
        normalized.profile = canonicalize_value(&normalized.profile)?;
        let serialized = serde_json::to_vec(&normalized).map_err(|error| {
            VideoError::new(
                VideoErrorCode::InvalidCacheInput,
                format!("could not serialize cache input: {error}"),
            )
        })?;
        let mut digest = Sha256::new();
        digest.update(CACHE_KEY_NAMESPACE);
        digest.update((serialized.len() as u64).to_be_bytes());
        digest.update(serialized);
        let bytes = digest.finalize();
        let mut encoded = String::with_capacity(64);
        for byte in bytes {
            write!(&mut encoded, "{byte:02x}")
                .expect("writing hexadecimal into String cannot fail");
        }
        CacheKey::parse(encoded)
    }
}

#[derive(Clone, Debug)]
pub struct CacheKeyBuilder {
    input: CacheKeyInput,
}

impl CacheKeyBuilder {
    pub fn new(stage: CacheStage, stage_version: impl Into<String>) -> Self {
        Self {
            input: CacheKeyInput {
                stage,
                stage_version: stage_version.into(),
                scene_id: None,
                input_artifacts: Vec::new(),
                tool_versions: BTreeMap::new(),
                manifest_slice: Value::Object(Map::new()),
                profile: Value::Object(Map::new()),
            },
        }
    }

    pub fn for_scene(mut self, scene_id: impl Into<String>) -> Self {
        self.input.scene_id = Some(scene_id.into());
        self
    }

    pub fn artifact(mut self, role: impl Into<String>, sha256: impl Into<String>) -> Self {
        self.input.input_artifacts.push(CacheArtifactInput {
            role: role.into(),
            sha256: sha256.into(),
        });
        self
    }

    pub fn tool_version(mut self, tool: impl Into<String>, version: impl Into<String>) -> Self {
        self.input.tool_versions.insert(tool.into(), version.into());
        self
    }

    pub fn manifest_slice(mut self, value: Value) -> Self {
        self.input.manifest_slice = value;
        self
    }

    pub fn profile(mut self, value: Value) -> Self {
        self.input.profile = value;
        self
    }

    pub fn build(self) -> VideoResult<CacheKey> {
        self.input.key()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestChange {
    Source,
    Transcript,
    ClipSelection,
    ScenePlan,
    SceneScript { scene_id: String },
    Voice { scene_id: String },
    Music { scene_id: Option<String> },
    CaptionContent { scene_id: Option<String> },
    CaptionStyle,
    Layout { scene_id: Option<String> },
    AudioMix { scene_id: Option<String> },
    Tracking { scene_id: Option<String> },
    RenderProfile,
    ProvenanceOnly,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvalidationPlan {
    /// Invalidates the stage for every scene and for non-scene artifacts.
    pub global_stages: BTreeSet<CacheStage>,
    /// Invalidates only keyed scene segments. Aggregate renders are added globally.
    pub scene_stages: BTreeMap<String, BTreeSet<CacheStage>>,
}

impl InvalidationPlan {
    pub fn for_changes(changes: impl IntoIterator<Item = ManifestChange>) -> VideoResult<Self> {
        let mut plan = Self::default();
        for change in changes {
            plan.apply(change)?;
        }
        Ok(plan)
    }

    pub fn invalidates(&self, stage: CacheStage, scene_id: Option<&str>) -> bool {
        self.global_stages.contains(&stage)
            || scene_id.is_some_and(|scene_id| {
                self.scene_stages
                    .get(scene_id)
                    .is_some_and(|stages| stages.contains(&stage))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.global_stages.is_empty() && self.scene_stages.values().all(BTreeSet::is_empty)
    }

    fn apply(&mut self, change: ManifestChange) -> VideoResult<()> {
        use CacheStage as Stage;
        match change {
            ManifestChange::Source => self.add_global([
                Stage::Probe,
                Stage::Proxy,
                Stage::Thumbnail,
                Stage::Waveform,
                Stage::Transcription,
                Stage::SceneAnalysis,
                Stage::Plan,
                Stage::Tracking,
                Stage::Captions,
                Stage::AudioMix,
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::Transcript => self.add_global([
                Stage::SceneAnalysis,
                Stage::Plan,
                Stage::Captions,
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::ClipSelection => self.add_global([
                Stage::Plan,
                Stage::Tracking,
                Stage::Captions,
                Stage::AudioMix,
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::ScenePlan => self.add_global([
                Stage::Plan,
                Stage::Speech,
                Stage::Music,
                Stage::Image,
                Stage::Tracking,
                Stage::Captions,
                Stage::AudioMix,
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::SceneScript { scene_id } => {
                self.add_scene(
                    &scene_id,
                    [Stage::Speech, Stage::Captions, Stage::SceneRender],
                )?;
                self.add_aggregate_renders();
            }
            ManifestChange::Voice { scene_id } => {
                self.add_scene(
                    &scene_id,
                    [Stage::Speech, Stage::AudioMix, Stage::SceneRender],
                )?;
                self.add_aggregate_renders();
            }
            ManifestChange::Music { scene_id } => match scene_id {
                Some(scene_id) => {
                    self.add_scene(
                        &scene_id,
                        [Stage::Music, Stage::AudioMix, Stage::SceneRender],
                    )?;
                    self.add_aggregate_renders();
                }
                None => self.add_global([
                    Stage::Music,
                    Stage::AudioMix,
                    Stage::SceneRender,
                    Stage::PreviewRender,
                    Stage::FinalRender,
                    Stage::PublishPackage,
                ]),
            },
            ManifestChange::CaptionContent { scene_id } => match scene_id {
                Some(scene_id) => {
                    self.add_scene(&scene_id, [Stage::Captions, Stage::SceneRender])?;
                    self.add_aggregate_renders();
                }
                None => self.add_global([
                    Stage::Captions,
                    Stage::SceneRender,
                    Stage::PreviewRender,
                    Stage::FinalRender,
                    Stage::PublishPackage,
                ]),
            },
            ManifestChange::CaptionStyle => self.add_global([
                Stage::Captions,
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::Layout { scene_id } => match scene_id {
                Some(scene_id) => {
                    self.add_scene(&scene_id, [Stage::Tracking, Stage::SceneRender])?;
                    self.add_aggregate_renders();
                }
                None => self.add_global([
                    Stage::Tracking,
                    Stage::SceneRender,
                    Stage::PreviewRender,
                    Stage::FinalRender,
                    Stage::PublishPackage,
                ]),
            },
            ManifestChange::AudioMix { scene_id } => match scene_id {
                Some(scene_id) => {
                    self.add_scene(&scene_id, [Stage::AudioMix, Stage::SceneRender])?;
                    self.add_aggregate_renders();
                }
                None => self.add_global([
                    Stage::AudioMix,
                    Stage::SceneRender,
                    Stage::PreviewRender,
                    Stage::FinalRender,
                    Stage::PublishPackage,
                ]),
            },
            ManifestChange::Tracking { scene_id } => match scene_id {
                Some(scene_id) => {
                    self.add_scene(&scene_id, [Stage::Tracking, Stage::SceneRender])?;
                    self.add_aggregate_renders();
                }
                None => self.add_global([
                    Stage::Tracking,
                    Stage::SceneRender,
                    Stage::PreviewRender,
                    Stage::FinalRender,
                    Stage::PublishPackage,
                ]),
            },
            ManifestChange::RenderProfile => self.add_global([
                Stage::SceneRender,
                Stage::PreviewRender,
                Stage::FinalRender,
                Stage::PublishPackage,
            ]),
            ManifestChange::ProvenanceOnly => self.add_global([Stage::PublishPackage]),
        }
        Ok(())
    }

    fn add_global(&mut self, stages: impl IntoIterator<Item = CacheStage>) {
        self.global_stages.extend(stages);
    }

    fn add_scene(
        &mut self,
        scene_id: &str,
        stages: impl IntoIterator<Item = CacheStage>,
    ) -> VideoResult<()> {
        validate_identifier(scene_id, "manifest_change.scene_id")?;
        self.scene_stages
            .entry(scene_id.to_owned())
            .or_default()
            .extend(stages);
        Ok(())
    }

    fn add_aggregate_renders(&mut self) {
        self.add_global([
            CacheStage::PreviewRender,
            CacheStage::FinalRender,
            CacheStage::PublishPackage,
        ]);
    }
}

fn validate_cache_input(input: &CacheKeyInput) -> VideoResult<()> {
    if input.stage_version.trim().is_empty() || input.stage_version.len() > 128 {
        return Err(VideoError::new(
            VideoErrorCode::InvalidCacheInput,
            "cache stage_version must be non-empty and at most 128 bytes",
        ));
    }
    if let Some(scene_id) = &input.scene_id {
        validate_identifier(scene_id, "cache.scene_id")?;
    }
    for artifact in &input.input_artifacts {
        if artifact.role.trim().is_empty() || artifact.role.len() > 128 {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCacheInput,
                "cache artifact role is empty or too large",
            ));
        }
        validate_sha256(
            &artifact.sha256,
            "cache.input_artifacts.sha256",
            VideoErrorCode::InvalidCacheInput,
        )?;
    }
    for (tool, version) in &input.tool_versions {
        if tool.trim().is_empty()
            || tool.len() > 128
            || version.trim().is_empty()
            || version.len() > 256
        {
            return Err(VideoError::new(
                VideoErrorCode::InvalidCacheInput,
                "cache tool name/version is empty or too large",
            ));
        }
    }
    canonicalize_value(&input.manifest_slice)?;
    canonicalize_value(&input.profile)?;
    Ok(())
}

fn canonicalize_value(value: &Value) -> VideoResult<Value> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
        Value::Number(number) if number.is_i64() || number.is_u64() => Ok(value.clone()),
        Value::Number(_) => Err(VideoError::new(
            VideoErrorCode::InvalidCacheInput,
            "floating-point cache inputs are forbidden; use integer fixed-point values",
        )),
        Value::Array(values) => values
            .iter()
            .map(canonicalize_value)
            .collect::<VideoResult<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => {
            let sorted: BTreeMap<&str, &Value> = values
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect();
            let mut canonical = Map::new();
            for (key, value) in sorted {
                canonical.insert(key.to_owned(), canonicalize_value(value)?);
            }
            Ok(Value::Object(canonical))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    #[test]
    fn canonical_key_ignores_object_and_artifact_insertion_order() {
        let left = CacheKeyBuilder::new(CacheStage::SceneRender, "scene-v2")
            .for_scene("scene-1")
            .artifact("audio", hash('a'))
            .artifact("video", hash('b'))
            .tool_version("ffmpeg", "8.0.1")
            .manifest_slice(json!({"layout": {"height": 1920, "width": 1080}, "script": "hi"}))
            .profile(json!({"crf": 30, "codec": "h264_nvenc"}))
            .build()
            .unwrap();

        let mut layout = Map::new();
        layout.insert("width".into(), json!(1080));
        layout.insert("height".into(), json!(1920));
        let mut slice = Map::new();
        slice.insert("script".into(), json!("hi"));
        slice.insert("layout".into(), Value::Object(layout));
        let right = CacheKeyBuilder::new(CacheStage::SceneRender, "scene-v2")
            .for_scene("scene-1")
            .artifact("video", hash('b'))
            .artifact("audio", hash('a'))
            .tool_version("ffmpeg", "8.0.1")
            .manifest_slice(Value::Object(slice))
            .profile(json!({"codec": "h264_nvenc", "crf": 30}))
            .build()
            .unwrap();

        assert_eq!(left, right);
        assert_eq!(left.as_str().len(), 64);
    }

    #[test]
    fn tool_or_array_order_changes_key() {
        let base = CacheKeyBuilder::new(CacheStage::Proxy, "1")
            .artifact("source", hash('a'))
            .tool_version("ffmpeg", "8.0.1")
            .profile(json!({"filters": ["scale", "fps"]}))
            .build()
            .unwrap();
        let changed_tool = CacheKeyBuilder::new(CacheStage::Proxy, "1")
            .artifact("source", hash('a'))
            .tool_version("ffmpeg", "8.0.2")
            .profile(json!({"filters": ["scale", "fps"]}))
            .build()
            .unwrap();
        let changed_order = CacheKeyBuilder::new(CacheStage::Proxy, "1")
            .artifact("source", hash('a'))
            .tool_version("ffmpeg", "8.0.1")
            .profile(json!({"filters": ["fps", "scale"]}))
            .build()
            .unwrap();
        assert_ne!(base, changed_tool);
        assert_ne!(base, changed_order);
    }

    #[test]
    fn floating_point_cache_inputs_are_rejected() {
        let error = CacheKeyBuilder::new(CacheStage::Proxy, "1")
            .profile(json!({"quality": 0.75}))
            .build()
            .unwrap_err();
        assert_eq!(error.code, VideoErrorCode::InvalidCacheInput);
    }

    #[test]
    fn scene_revision_only_invalidates_that_segment_and_aggregates() {
        let plan = InvalidationPlan::for_changes([ManifestChange::SceneScript {
            scene_id: "scene-2".into(),
        }])
        .unwrap();
        assert!(plan.invalidates(CacheStage::Speech, Some("scene-2")));
        assert!(plan.invalidates(CacheStage::SceneRender, Some("scene-2")));
        assert!(!plan.invalidates(CacheStage::SceneRender, Some("scene-1")));
        assert!(!plan.invalidates(CacheStage::Music, Some("scene-2")));
        assert!(plan.invalidates(CacheStage::PreviewRender, None));
        assert!(plan.invalidates(CacheStage::FinalRender, Some("scene-1")));
    }

    #[test]
    fn provenance_change_does_not_destroy_media_caches() {
        let plan = InvalidationPlan::for_changes([ManifestChange::ProvenanceOnly]).unwrap();
        assert!(plan.invalidates(CacheStage::PublishPackage, None));
        assert!(!plan.invalidates(CacheStage::SceneRender, None));
        assert!(!plan.invalidates(CacheStage::Transcription, None));
    }

    #[test]
    fn source_change_invalidates_ingest_and_all_downstream_stages() {
        let plan = InvalidationPlan::for_changes([ManifestChange::Source]).unwrap();
        assert!(plan.invalidates(CacheStage::Probe, None));
        assert!(plan.invalidates(CacheStage::Transcription, None));
        assert!(plan.invalidates(CacheStage::FinalRender, Some("any-scene")));
    }
}
