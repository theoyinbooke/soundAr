//! One production, many deliverables.
//!
//! A finished episode is not one file. It is an audio episode with chapters, a video master, a
//! short vertical cut for social, a transcript, and show notes - and producing those by hand is
//! where most of the work of publishing actually goes.
//!
//! The interesting part is the trailer. soundAr already has a deterministic analyst that finds the
//! strongest moments in an *imported* transcript. An episode soundAr wrote knows exactly what every
//! line says and exactly how long its take runs, so that same analyst can be pointed at the
//! episode's own narration and pick the pull-quote. Nothing here estimates or guesses: a turn
//! contributes to the transcript only when it has a published take with a measured duration, so the
//! trailer is cut from the same clock the master is rendered on.

use super::contracts::{
    validate_identifier, validate_nonempty, Microseconds, RenderArtifactRole, TimeRange,
    TranscriptSegment, TranscriptTimingSource, TranscriptVersion, Validate, VideoError,
    VideoErrorCode, VideoProjectManifest, VideoResult,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A trailer long enough to land a moment and short enough to finish.
pub const TRAILER_MINIMUM_US: i64 = 8_000_000;
pub const TRAILER_TARGET_US: i64 = 30_000_000;
pub const TRAILER_MAXIMUM_US: i64 = 60_000_000;

/// What a finished release contains.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseMemberKind {
    /// The episode as audio, with chapter marks.
    PodcastAudio,
    /// The rendered video master.
    VideoMaster,
    /// A short vertical cut of the strongest moment.
    Trailer,
    /// A square waveform video, for feeds where only video plays.
    Audiogram,
    /// The episode's own transcript.
    Transcript,
    /// Written notes for the episode.
    ShowNotes,
}

impl ReleaseMemberKind {
    pub const ALL: [Self; 6] = [
        Self::PodcastAudio,
        Self::VideoMaster,
        Self::Trailer,
        Self::Audiogram,
        Self::Transcript,
        Self::ShowNotes,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PodcastAudio => "podcast_audio",
            Self::VideoMaster => "video_master",
            Self::Trailer => "trailer",
            Self::Audiogram => "audiogram",
            Self::Transcript => "transcript",
            Self::ShowNotes => "show_notes",
        }
    }
}

/// Why a member cannot be produced yet.
///
/// Readiness is reported rather than silently skipped: a release that quietly omits its trailer
/// looks identical to one that never wanted a trailer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseMemberPlan {
    pub kind: ReleaseMemberKind,
    pub ready: bool,
    /// Present when `ready` is false. Names the exact missing prerequisite.
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

/// One chapter of the episode.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseChapter {
    pub id: String,
    pub title: String,
    pub start_us: Microseconds,
    pub end_us: Microseconds,
}

impl Validate for ReleaseChapter {
    fn validate(&self) -> VideoResult<()> {
        validate_identifier(&self.id, "release.chapters.id")?;
        validate_nonempty(&self.title, "release.chapters.title", 512)?;
        if self.end_us <= self.start_us {
            return Err(VideoError::new(
                VideoErrorCode::InvalidRelease,
                "a chapter must end after it begins",
            )
            .at("release.chapters.end_us"));
        }
        Ok(())
    }
}

/// Everything a release would contain, and what is still missing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePlan {
    pub members: Vec<ReleaseMemberPlan>,
    pub chapters: Vec<ReleaseChapter>,
    /// The moment the trailer would be cut from, when one can be chosen.
    #[serde(default)]
    pub trailer_range: Option<TimeRange>,
}

impl ReleasePlan {
    pub fn is_ready(&self) -> bool {
        self.members.iter().all(|member| member.ready)
    }

    pub fn blocked(&self) -> Vec<&ReleaseMemberPlan> {
        self.members.iter().filter(|member| !member.ready).collect()
    }
}

/// Build a source-clock transcript from the episode's own narration.
///
/// A turn appears only when it has a published take with a measured duration and a clip placing it
/// on the timeline. That is what keeps the result a measurement rather than an estimate: the ranges
/// come from the same clock the master renders on, so a moment chosen here is the same moment in
/// the finished file.
///
/// Returns `None` when nothing has been narrated yet, because a transcript of an unperformed script
/// would describe audio that does not exist.
pub fn episode_transcript(
    manifest: &VideoProjectManifest,
) -> VideoResult<Option<TranscriptVersion>> {
    let artifact_duration = manifest
        .render_artifacts
        .iter()
        .filter_map(|artifact| Some((artifact.id.as_str(), artifact.duration_us?)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let turn_text = manifest
        .dialogue
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect::<std::collections::BTreeMap<_, _>>();

    let mut segments = Vec::new();
    for track in &manifest.tracks {
        for clip in &track.clips {
            let Some(turn_id) = clip.turn_id.as_deref() else {
                continue;
            };
            let Some(turn) = turn_text.get(turn_id) else {
                continue;
            };
            // The take must exist and have been measured; a clip pointing at unmeasured media
            // cannot contribute an honest range.
            let Some(artifact_id) = clip.media.render_artifact_id.as_deref() else {
                continue;
            };
            if !artifact_duration.contains_key(artifact_id) {
                continue;
            }
            let end = clip
                .timeline_start_us
                .checked_add(clip.timeline_duration_us)?;
            segments.push(TranscriptSegment {
                id: format!("episode-{turn_id}"),
                range: TimeRange::new(clip.timeline_start_us.0, end.0)?,
                text: turn.text.clone(),
                speaker_id: Some(turn.character_id.clone()),
                word_ids: Vec::new(),
            });
        }
    }
    if segments.is_empty() {
        return Ok(None);
    }
    segments.sort_by_key(|segment| (segment.range.start_us, segment.range.end_us));

    let mut hasher = Sha256::new();
    for segment in &segments {
        hasher.update(segment.id.as_bytes());
        hasher.update([0x01]);
        hasher.update(segment.text.as_bytes());
        hasher.update([0x02]);
    }
    let transcript = TranscriptVersion {
        id: format!("episode-transcript-{}", manifest.project_id),
        // The episode is its own source: these ranges are on the project clock the master renders
        // on, not on an imported file's clock.
        source_asset_id: manifest.project_id.clone(),
        source_clock_duration_us: manifest.timeline_duration_us,
        language: manifest.cast.first().map(|member| member.language.clone()),
        // The words were written, not heard. Claiming a recognizer produced them would misreport
        // where this timing came from.
        timing_source: TranscriptTimingSource::Manual,
        preserved_source_gaps: true,
        segments,
        words: Vec::new(),
        content_sha256: format!("{:x}", hasher.finalize()),
        created_at: manifest.updated_at.clone(),
    };
    transcript.validate()?;
    Ok(Some(transcript))
}

/// Chapters for the audio episode.
///
/// Scenes are the author's own divisions of the episode, so they are the chapters. An episode with
/// no scenes has no chapters rather than an invented one per line.
pub fn episode_chapters(manifest: &VideoProjectManifest) -> VideoResult<Vec<ReleaseChapter>> {
    let mut chapters = Vec::with_capacity(manifest.reviewed_scenes.len());
    for scene in &manifest.reviewed_scenes {
        let end = scene
            .timeline_start_us
            .checked_add(scene.timeline_duration_us)?;
        let chapter = ReleaseChapter {
            id: format!("chapter-{}", scene.id),
            title: scene.title.clone(),
            start_us: scene.timeline_start_us,
            end_us: end,
        };
        chapter.validate()?;
        chapters.push(chapter);
    }
    chapters.sort_by_key(|chapter| chapter.start_us);
    Ok(chapters)
}

/// Render the episode's chapters as an FFmetadata document.
///
/// This is what a podcast player reads to offer chapter navigation. The microsecond timebase is
/// declared explicitly rather than converting to milliseconds, so a chapter mark lands on the same
/// clock the episode was assembled on.
///
/// FFmetadata treats `=`, `;`, `#`, `\`, and a newline as syntax, so a chapter title containing any
/// of them is escaped. An unescaped title would either truncate the document or silently move a
/// chapter's boundary.
pub fn ffmetadata_chapters(chapters: &[ReleaseChapter]) -> String {
    let mut document = String::from(";FFMETADATA1\n");
    for chapter in chapters {
        document.push_str("[CHAPTER]\nTIMEBASE=1/1000000\n");
        document.push_str(&format!("START={}\n", chapter.start_us.0));
        document.push_str(&format!("END={}\n", chapter.end_us.0));
        document.push_str(&format!("title={}\n", escape_ffmetadata(&chapter.title)));
    }
    document
}

fn escape_ffmetadata(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '=' | ';' | '#' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\n' => escaped.push_str("\\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Decide what this episode's release would contain.
///
/// `has_show_notes` is supplied by the caller because notes are written, not derived: the contract
/// can record whether they exist but has no business inventing them.
pub fn plan_release(
    manifest: &VideoProjectManifest,
    trailer_range: Option<TimeRange>,
    has_show_notes: bool,
) -> VideoResult<ReleasePlan> {
    let transcript = episode_transcript(manifest)?;
    let has_master = manifest.render_artifacts.iter().any(|artifact| {
        matches!(artifact.role, RenderArtifactRole::FinalMaster)
            && matches!(
                artifact.publication_state,
                super::contracts::PublicationState::Published
            )
    });
    let narrated = transcript.is_some();
    // A stand-in take must never leave soundAr as the finished episode.
    let drafts = manifest.draft_turn_ids();
    let no_drafts = drafts.is_empty();
    let draft_reason = format!(
        "{} line(s) are still draft takes; promote them to final first",
        drafts.len()
    );

    let member = |kind: ReleaseMemberKind, ready: bool, reason: &str| ReleaseMemberPlan {
        kind,
        ready,
        blocked_reason: if ready {
            None
        } else {
            Some(reason.to_string())
        },
    };

    let members = vec![
        member(
            ReleaseMemberKind::PodcastAudio,
            narrated && no_drafts,
            if narrated {
                &draft_reason
            } else {
                "No line has been narrated yet, so there is no audio episode to publish"
            },
        ),
        member(
            ReleaseMemberKind::VideoMaster,
            has_master && no_drafts,
            if has_master {
                &draft_reason
            } else {
                "Render a final master for the current timeline first"
            },
        ),
        member(
            ReleaseMemberKind::Trailer,
            trailer_range.is_some(),
            "No narrated moment is long enough to cut a trailer from",
        ),
        member(
            ReleaseMemberKind::Audiogram,
            has_master && no_drafts,
            if has_master {
                &draft_reason
            } else {
                "Render a final master for the current timeline first"
            },
        ),
        member(
            ReleaseMemberKind::Transcript,
            narrated,
            "No line has been narrated yet, so there is nothing to transcribe",
        ),
        member(
            ReleaseMemberKind::ShowNotes,
            has_show_notes,
            "Write the episode's show notes first",
        ),
    ];

    Ok(ReleasePlan {
        members,
        chapters: episode_chapters(manifest)?,
        trailer_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::cast::{CastDelivery, CastMember, DialogueTurn};
    use crate::video::contracts::{
        AudioMix, CanvasMode, CanvasSpec, LayoutPlan, MediaReference, NormalizedRect,
        PublicationState, RationalFrameRate, RationalRate, RenderArtifact, ReviewState,
        ReviewedScene, TimelineClip, TimelineTrack, TrackKind,
    };

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn manifest() -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "project-release",
            "Episode 1",
            RationalFrameRate::FPS_30,
            Microseconds(60_000_000),
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
        for (id, name) in [("narrator", "NARRATOR"), ("adaeze", "ADAEZE")] {
            manifest.cast.push(CastMember {
                id: id.into(),
                name: name.into(),
                display_name: name.into(),
                voice_id: "af-heart".into(),
                model_id: "hexgrad/Kokoro-82M".into(),
                language: "en-US".into(),
                delivery: CastDelivery::default(),
                consent_reference_id: None,
                notes: None,
                created_at: NOW.into(),
            });
        }
        manifest
    }

    /// Narrate `count` turns, each with a published take and a clip on the project clock.
    fn narrate(manifest: &mut VideoProjectManifest, count: usize) {
        let mut clips = Vec::new();
        for index in 0..count {
            let turn_id = format!("turn-{index}");
            let artifact_id = format!("take-{index}");
            manifest.dialogue.push(DialogueTurn {
                id: turn_id.clone(),
                scene_id: None,
                order: index as u32,
                character_id: if index % 2 == 0 { "narrator" } else { "adaeze" }.into(),
                text: format!("This is line number {index} of the episode."),
                direction: None,
                source_line: index as u32 + 1,
                revision: 1,
            });
            manifest.render_artifacts.push(RenderArtifact {
                id: artifact_id.clone(),
                role: RenderArtifactRole::SceneSegment,
                scene_id: None,
                managed_path: format!("renders/{artifact_id}.wav"),
                sha256: format!("{index:064}"),
                cache_key: format!("{:064}", index + 500),
                mime_type: "audio/wav".into(),
                duration_us: Some(Microseconds(5_000_000)),
                width: None,
                height: None,
                publication_state: PublicationState::Published,
                created_at: NOW.into(),
            });
            clips.push(TimelineClip {
                id: format!("clip-{index}"),
                scene_id: None,
                turn_id: Some(turn_id),
                media: MediaReference {
                    source_asset_id: None,
                    render_artifact_id: Some(artifact_id),
                },
                source_range: TimeRange::new(0, 5_000_000).unwrap(),
                timeline_start_us: Microseconds(index as i64 * 5_000_000),
                timeline_duration_us: Microseconds(5_000_000),
                playback_rate: RationalRate::ONE,
                gain_db_milli: 0,
                muted: false,
                crop: None,
            });
        }
        manifest.tracks.push(TimelineTrack {
            id: "speech".into(),
            kind: TrackKind::Audio,
            clips,
            preserve_gaps: false,
        });
    }

    #[test]
    fn an_episode_transcribes_only_what_it_actually_performed() {
        let mut unperformed = manifest();
        // A written but unnarrated script describes audio that does not exist.
        unperformed.dialogue.push(DialogueTurn {
            id: "turn-unread".into(),
            scene_id: None,
            order: 0,
            character_id: "narrator".into(),
            text: "Never performed.".into(),
            direction: None,
            source_line: 1,
            revision: 1,
        });
        assert!(episode_transcript(&unperformed).unwrap().is_none());

        let mut narrated = manifest();
        narrate(&mut narrated, 3);
        let transcript = episode_transcript(&narrated).unwrap().expect("transcript");
        assert_eq!(transcript.segments.len(), 3);
        assert_eq!(transcript.segments[0].range.start_us, Microseconds::ZERO);
        assert_eq!(
            transcript.segments[2].range.end_us,
            Microseconds(15_000_000)
        );
        assert_eq!(transcript.segments[1].speaker_id.as_deref(), Some("adaeze"));
        // The words were written, not heard, so the timing source says so.
        assert_eq!(transcript.timing_source, TranscriptTimingSource::Manual);
    }

    #[test]
    fn a_turn_with_no_measured_take_does_not_reach_the_transcript() {
        let mut manifest = manifest();
        narrate(&mut manifest, 2);
        // Strip the measurement from the second take.
        manifest.render_artifacts[1].duration_us = None;
        let transcript = episode_transcript(&manifest).unwrap().expect("transcript");
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].id, "episode-turn-0");
    }

    #[test]
    fn the_existing_analyst_can_choose_a_moment_from_generated_work() {
        use crate::video::intelligence::{identify_clip_candidates, CandidatePolicy};

        let mut manifest = manifest();
        narrate(&mut manifest, 8);
        let transcript = episode_transcript(&manifest).unwrap().expect("transcript");

        // The same deterministic analyst soundAr uses on imported source, pointed at the episode's
        // own narration. This is what makes an automatic trailer possible without a second system.
        let analysis = identify_clip_candidates(
            &transcript,
            &CandidatePolicy {
                minimum_duration_us: Microseconds(TRAILER_MINIMUM_US),
                target_duration_us: Microseconds(TRAILER_TARGET_US),
                maximum_duration_us: Microseconds(TRAILER_MAXIMUM_US),
                maximum_candidates: 4,
            },
            &std::collections::BTreeSet::new(),
        )
        .expect("analyze the episode's own narration");

        assert!(!analysis.candidates.is_empty());
        let candidate = &analysis.candidates[0];
        let span = candidate.source_range.end_us.0 - candidate.source_range.start_us.0;
        assert!(span >= TRAILER_MINIMUM_US, "trailer candidate is too short");
        assert!(span <= TRAILER_MAXIMUM_US, "trailer candidate is too long");
        // The chosen moment sits on the same clock the master renders on.
        assert!(candidate.source_range.end_us <= manifest.timeline_duration_us);
    }

    #[test]
    fn scenes_are_the_episodes_chapters() {
        let mut chaptered = manifest();
        for (index, title) in ["The letter", "The answer"].into_iter().enumerate() {
            chaptered.reviewed_scenes.push(ReviewedScene {
                id: format!("scene-{index}"),
                candidate_id: None,
                source_asset_id: None,
                source_range: None,
                timeline_start_us: Microseconds(index as i64 * 20_000_000),
                timeline_duration_us: Microseconds(20_000_000),
                title: title.into(),
                script: title.into(),
                review_state: ReviewState::Approved,
                revision: 1,
            });
        }
        let chapters = episode_chapters(&chaptered).unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "The letter");
        assert_eq!(chapters[1].start_us, Microseconds(20_000_000));

        // An episode with no scenes has no chapters rather than one invented per line.
        let bare = manifest();
        assert!(episode_chapters(&bare).unwrap().is_empty());
    }

    #[test]
    fn a_stand_in_take_can_never_leave_soundar_as_the_finished_episode() {
        use crate::video::contracts::TakeFidelity;

        let mut manifest = manifest();
        narrate(&mut manifest, 8);
        // Bind every performed line to a take, then mark one of them a stand-in.
        for index in 0..8usize {
            manifest
                .narration_bindings
                .push(crate::video::contracts::NarrationBinding {
                    id: format!("binding-{index}"),
                    scene_id: None,
                    turn_id: Some(format!("turn-{index}")),
                    lexicon_fingerprint: None,
                    fidelity: if index == 3 {
                        TakeFidelity::Draft
                    } else {
                        TakeFidelity::Final
                    },
                    render_artifact_id: format!("take-{index}"),
                    history_id: format!("history-{index}"),
                    generation_job_id: format!("job-{index}"),
                    voice_id: "af-heart".into(),
                    model_id: "hexgrad/Kokoro-82M".into(),
                    speaker: if index % 2 == 0 { "NARRATOR" } else { "ADAEZE" }.into(),
                    language: "en-US".into(),
                    script_sha256: format!(
                        "{:x}",
                        <sha2::Sha256 as sha2::Digest>::digest(
                            format!("This is line number {index} of the episode.").as_bytes()
                        )
                    ),
                    created_at: NOW.into(),
                });
        }
        manifest.validate_strict().unwrap();

        let trailer = TimeRange::new(0, TRAILER_TARGET_US).unwrap();
        let plan = plan_release(&manifest, Some(trailer), true).unwrap();
        let blocked = plan
            .blocked()
            .into_iter()
            .map(|member| member.kind)
            .collect::<Vec<_>>();
        assert!(blocked.contains(&ReleaseMemberKind::PodcastAudio));
        // The reason names the count so the author knows how much is left to re-read.
        assert!(plan.members[0]
            .blocked_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("1 line(s)")));
    }

    #[test]
    fn chapters_are_written_on_the_microsecond_clock_they_were_assembled_on() {
        let chapters = vec![
            ReleaseChapter {
                id: "chapter-one".into(),
                title: "The letter".into(),
                start_us: Microseconds::ZERO,
                end_us: Microseconds(20_000_000),
            },
            ReleaseChapter {
                id: "chapter-two".into(),
                title: "The answer".into(),
                start_us: Microseconds(20_000_000),
                end_us: Microseconds(45_500_000),
            },
        ];
        let document = ffmetadata_chapters(&chapters);
        assert!(document.starts_with(";FFMETADATA1\n"));
        assert_eq!(document.matches("[CHAPTER]").count(), 2);
        assert!(document.contains("TIMEBASE=1/1000000"));
        assert!(document.contains("START=20000000\nEND=45500000\ntitle=The answer"));
    }

    #[test]
    fn a_chapter_title_cannot_break_the_metadata_document() {
        // These characters are FFmetadata syntax; unescaped they would truncate the document or
        // silently move a chapter boundary.
        let chapters = vec![ReleaseChapter {
            id: "chapter-one".into(),
            title: "Act 1; scene #2 = the reveal".into(),
            start_us: Microseconds::ZERO,
            end_us: Microseconds(1_000_000),
        }];
        let document = ffmetadata_chapters(&chapters);
        assert!(
            document.contains(r"title=Act 1\; scene \#2 \= the reveal"),
            "{document}"
        );
        // Exactly one key per line: nothing leaked into a new directive.
        assert_eq!(
            document
                .lines()
                .filter(|line| line.starts_with("title="))
                .count(),
            1
        );
    }

    #[test]
    fn an_episode_with_no_chapters_still_produces_a_valid_document() {
        assert_eq!(ffmetadata_chapters(&[]), ";FFMETADATA1\n");
    }

    #[test]
    fn a_release_names_what_is_missing_rather_than_quietly_omitting_it() {
        let bare = manifest();
        let plan = plan_release(&bare, None, false).unwrap();
        assert!(!plan.is_ready());
        assert_eq!(plan.blocked().len(), ReleaseMemberKind::ALL.len());
        for member in plan.blocked() {
            assert!(
                member.blocked_reason.is_some(),
                "{:?} is blocked without saying why",
                member.kind
            );
        }
    }

    #[test]
    fn a_release_is_ready_only_when_every_member_can_be_produced() {
        let mut manifest = manifest();
        narrate(&mut manifest, 8);
        manifest.render_artifacts.push(RenderArtifact {
            id: "master".into(),
            role: RenderArtifactRole::FinalMaster,
            scene_id: None,
            managed_path: "renders/master.mp4".into(),
            sha256: "f".repeat(64),
            cache_key: "e".repeat(64),
            mime_type: "video/mp4".into(),
            duration_us: Some(Microseconds(40_000_000)),
            width: Some(1080),
            height: Some(1920),
            publication_state: PublicationState::Published,
            created_at: NOW.into(),
        });

        let trailer = TimeRange::new(0, TRAILER_TARGET_US).unwrap();
        let plan = plan_release(&manifest, Some(trailer), true).unwrap();
        assert!(plan.is_ready(), "blocked: {:?}", plan.blocked());
        assert_eq!(plan.trailer_range, Some(trailer));

        // A staged master is not a published one.
        let mut staged = manifest.clone();
        staged
            .render_artifacts
            .last_mut()
            .unwrap()
            .publication_state = PublicationState::Staged;
        let plan = plan_release(&staged, Some(trailer), true).unwrap();
        assert!(!plan.is_ready());
        assert_eq!(plan.blocked()[0].kind, ReleaseMemberKind::VideoMaster);
    }
}
