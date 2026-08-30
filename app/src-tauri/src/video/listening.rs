//! What the episode actually turned out to be.
//!
//! Until now the assistant revises from the plan it wrote. That is the wrong source: the plan says
//! what was intended, and the manifest says what was asked for, but neither says what the rendered
//! episode is. "The second act drags" is a judgment about a thing nobody has examined.
//!
//! This module reports the episode as rendered - which lines were performed, how long each one
//! actually runs, how the speaking time divides between characters, and where the silences fall -
//! so a revision is a response to something measured.
//!
//! Every number here comes from a published artifact with a measured duration. Nothing is
//! estimated from word counts or reading speed. A value that was never measured is absent rather
//! than approximated, because an approximation the assistant cannot distinguish from a measurement
//! is worse than no value at all.

use super::contracts::{Microseconds, TrackKind, VideoProjectManifest, VideoResult};
use super::quality::LoudnessMeasurement;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Basis points, so a share is exact without carrying a float through the contract.
pub const SHARE_BASIS_POINTS: i64 = 10_000;

/// How much of the episode one character actually speaks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerShare {
    pub character_id: String,
    pub display_name: String,
    pub narrated_turns: usize,
    /// Measured from the takes, not estimated from the words.
    pub spoken_us: Microseconds,
    /// Share of all spoken time, in basis points.
    pub share_bp: i64,
}

/// Where the silences fall.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GapSummary {
    pub count: usize,
    pub total_us: Microseconds,
    pub longest_us: Microseconds,
    /// The middle gap. More useful than a mean, which one long pause distorts.
    pub median_us: Microseconds,
}

/// The episode as rendered.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeListening {
    pub project_id: String,
    pub timeline_duration_us: Microseconds,
    /// Total speaking time across every performed line.
    pub spoken_us: Microseconds,
    pub narrated_turns: usize,
    /// Lines that are written but not performed. These are why a summary may not describe the whole
    /// script.
    pub unnarrated_turns: Vec<String>,
    pub speakers: Vec<SpeakerShare>,
    pub gaps: GapSummary,
    /// Each performed line with its measured position and length, in timeline order.
    pub lines: Vec<ListenedLine>,
    pub music_cues_placed: usize,
    pub music_cues_planned: usize,
    pub sound_placements: usize,
    /// Lines still standing in with a draft take. An episode with any of these is unfinished.
    pub draft_turns: Vec<String>,
    /// Present only when the runtime measured it. Absent means unmeasured, never "fine".
    #[serde(default)]
    pub loudness: Option<LoudnessMeasurement>,
}

/// One performed line, as it actually sits in the episode.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListenedLine {
    pub turn_id: String,
    pub character_id: String,
    pub text: String,
    pub start_us: Microseconds,
    pub duration_us: Microseconds,
    /// Silence between the previous line and this one, as rendered.
    pub lead_in_us: Microseconds,
}

/// Describe the episode as rendered.
///
/// A line is included only when it has a published take with a measured duration and a clip placing
/// it on the timeline. Everything else about it - that it exists, that it was written - is reported
/// through `unnarrated_turns` rather than folded into numbers that would then describe audio nobody
/// has.
pub fn listen_to_episode(
    manifest: &VideoProjectManifest,
    loudness: Option<LoudnessMeasurement>,
) -> VideoResult<EpisodeListening> {
    let measured_artifacts = manifest
        .render_artifacts
        .iter()
        .filter(|artifact| artifact.duration_us.is_some())
        .map(|artifact| artifact.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let turn_by_id = manifest
        .dialogue
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect::<BTreeMap<_, _>>();

    let mut lines = Vec::new();
    for track in manifest
        .tracks
        .iter()
        .filter(|track| matches!(track.kind, TrackKind::Audio))
    {
        for clip in &track.clips {
            let Some(turn_id) = clip.turn_id.as_deref() else {
                continue;
            };
            let Some(turn) = turn_by_id.get(turn_id) else {
                continue;
            };
            if !clip
                .media
                .render_artifact_id
                .as_deref()
                .is_some_and(|id| measured_artifacts.contains(id))
            {
                continue;
            }
            lines.push(ListenedLine {
                turn_id: turn_id.to_string(),
                character_id: turn.character_id.clone(),
                text: turn.text.clone(),
                start_us: clip.timeline_start_us,
                duration_us: clip.timeline_duration_us,
                // Filled in below, once the lines are in timeline order.
                lead_in_us: Microseconds::ZERO,
            });
        }
    }
    lines.sort_by_key(|line| (line.start_us, line.duration_us));

    // The rendered lead-in, which is what a listener hears rather than what the beat asked for.
    let mut cursor = Microseconds::ZERO;
    let mut gaps = Vec::new();
    for line in &mut lines {
        let gap = line.start_us.0 - cursor.0;
        if gap > 0 {
            line.lead_in_us = Microseconds(gap);
            gaps.push(gap);
        }
        cursor = cursor.max(line.start_us.checked_add(line.duration_us)?);
    }

    let spoken_us = lines
        .iter()
        .try_fold(Microseconds::ZERO, |total, line| {
            total.checked_add(line.duration_us)
        })?;

    let display_names = manifest
        .cast
        .iter()
        .map(|member| (member.id.as_str(), member.display_name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut by_character: BTreeMap<&str, (usize, i64)> = BTreeMap::new();
    for line in &lines {
        let entry = by_character.entry(line.character_id.as_str()).or_default();
        entry.0 += 1;
        entry.1 += line.duration_us.0;
    }
    let mut speakers = by_character
        .into_iter()
        .map(|(character_id, (turns, total))| SpeakerShare {
            character_id: character_id.to_string(),
            display_name: display_names
                .get(character_id)
                .map_or_else(|| character_id.to_string(), |name| (*name).to_string()),
            narrated_turns: turns,
            spoken_us: Microseconds(total),
            share_bp: if spoken_us.0 > 0 {
                total.saturating_mul(SHARE_BASIS_POINTS) / spoken_us.0
            } else {
                0
            },
        })
        .collect::<Vec<_>>();
    // Loudest voice first: who dominates the episode is usually the first question asked of it.
    speakers.sort_by(|left, right| {
        right
            .spoken_us
            .cmp(&left.spoken_us)
            .then_with(|| left.character_id.cmp(&right.character_id))
    });

    gaps.sort_unstable();
    let gap_summary = GapSummary {
        count: gaps.len(),
        total_us: Microseconds(gaps.iter().sum()),
        longest_us: Microseconds(gaps.last().copied().unwrap_or_default()),
        median_us: Microseconds(gaps.get(gaps.len() / 2).copied().unwrap_or_default()),
    };

    let narrated = lines
        .iter()
        .map(|line| line.turn_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let unnarrated_turns = manifest
        .dialogue
        .iter()
        .filter(|turn| !narrated.contains(turn.id.as_str()))
        .map(|turn| turn.id.clone())
        .collect::<Vec<_>>();

    Ok(EpisodeListening {
        project_id: manifest.project_id.clone(),
        timeline_duration_us: manifest.timeline_duration_us,
        spoken_us,
        narrated_turns: lines.len(),
        unnarrated_turns,
        speakers,
        gaps: gap_summary,
        lines,
        music_cues_placed: manifest
            .music_cues
            .iter()
            .filter(|cue| !cue.needs_generation())
            .count(),
        music_cues_planned: manifest
            .music_cues
            .iter()
            .filter(|cue| cue.needs_generation())
            .count(),
        sound_placements: manifest.sound_layers.len(),
        draft_turns: manifest
            .draft_turn_ids()
            .into_iter()
            .map(str::to_string)
            .collect(),
        loudness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::cast::{CastDelivery, CastMember, DialogueTurn};
    use crate::video::contracts::{
        AudioMix, CanvasMode, CanvasSpec, LayoutPlan, MediaReference, NormalizedRect,
        PublicationState, RationalFrameRate, RationalRate, RenderArtifact, RenderArtifactRole,
        TimeRange, TimelineClip, TimelineTrack,
    };

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn manifest() -> VideoProjectManifest {
        let mut manifest = VideoProjectManifest::new(
            "project-listen",
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
                display_name: format!("{name} display"),
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

    /// `(character, start_us, duration_us, measured)` for each line.
    fn perform(manifest: &mut VideoProjectManifest, lines: &[(&str, i64, i64, bool)]) {
        let mut clips = Vec::new();
        for (index, (character, start, duration, measured)) in lines.iter().enumerate() {
            let turn_id = format!("turn-{index}");
            let artifact_id = format!("take-{index}");
            manifest.dialogue.push(DialogueTurn {
                id: turn_id.clone(),
                scene_id: None,
                order: index as u32,
                character_id: (*character).into(),
                text: format!("Line {index}."),
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
                cache_key: format!("{:064}", index + 700),
                mime_type: "audio/wav".into(),
                duration_us: measured.then_some(Microseconds(*duration)),
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
                source_range: TimeRange::new(0, *duration).unwrap(),
                timeline_start_us: Microseconds(*start),
                timeline_duration_us: Microseconds(*duration),
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
    fn speaking_time_is_measured_from_takes_not_estimated_from_words() {
        let mut manifest = manifest();
        perform(
            &mut manifest,
            &[
                ("narrator", 0, 6_000_000, true),
                ("adaeze", 6_500_000, 2_000_000, true),
            ],
        );
        let heard = listen_to_episode(&manifest, None).unwrap();

        assert_eq!(heard.spoken_us, Microseconds(8_000_000));
        assert_eq!(heard.narrated_turns, 2);
        // The narrator speaks three quarters of the episode despite the shorter line count.
        assert_eq!(heard.speakers[0].character_id, "narrator");
        assert_eq!(heard.speakers[0].share_bp, 7_500);
        assert_eq!(heard.speakers[1].share_bp, 2_500);
        assert_eq!(heard.speakers[0].display_name, "NARRATOR display");
    }

    #[test]
    fn a_line_with_no_measured_take_is_reported_as_unnarrated_rather_than_counted() {
        let mut manifest = manifest();
        perform(
            &mut manifest,
            &[
                ("narrator", 0, 6_000_000, true),
                // Rendered but never measured: it cannot contribute a duration.
                ("adaeze", 6_000_000, 2_000_000, false),
            ],
        );
        let heard = listen_to_episode(&manifest, None).unwrap();
        assert_eq!(heard.narrated_turns, 1);
        assert_eq!(heard.unnarrated_turns, vec!["turn-1".to_string()]);
        assert_eq!(heard.spoken_us, Microseconds(6_000_000));
        assert_eq!(heard.speakers.len(), 1);
    }

    #[test]
    fn the_reported_lead_in_is_the_silence_actually_rendered() {
        let mut manifest = manifest();
        perform(
            &mut manifest,
            &[
                ("narrator", 0, 5_000_000, true),
                ("adaeze", 6_000_000, 2_000_000, true),
                ("narrator", 12_000_000, 3_000_000, true),
            ],
        );
        let heard = listen_to_episode(&manifest, None).unwrap();
        assert_eq!(heard.lines[0].lead_in_us, Microseconds::ZERO);
        assert_eq!(heard.lines[1].lead_in_us, Microseconds(1_000_000));
        assert_eq!(heard.lines[2].lead_in_us, Microseconds(4_000_000));

        // The median resists one long pause in a way a mean would not.
        assert_eq!(heard.gaps.count, 2);
        assert_eq!(heard.gaps.longest_us, Microseconds(4_000_000));
        assert_eq!(heard.gaps.total_us, Microseconds(5_000_000));
    }

    #[test]
    fn an_unmeasured_master_reports_no_loudness_rather_than_a_guess() {
        let mut manifest = manifest();
        perform(&mut manifest, &[("narrator", 0, 5_000_000, true)]);

        assert!(listen_to_episode(&manifest, None).unwrap().loudness.is_none());

        let measured = LoudnessMeasurement {
            integrated_lufs_milli: -16_100,
            true_peak_db_milli: -1_400,
        };
        let heard = listen_to_episode(&manifest, Some(measured)).unwrap();
        assert_eq!(heard.loudness, Some(measured));
    }

    #[test]
    fn an_unperformed_script_reports_nothing_spoken() {
        let mut manifest = manifest();
        manifest.dialogue.push(DialogueTurn {
            id: "turn-unread".into(),
            scene_id: None,
            order: 0,
            character_id: "narrator".into(),
            text: "Never performed.".into(),
            direction: None,
            source_line: 1,
            revision: 1,
        });
        let heard = listen_to_episode(&manifest, None).unwrap();
        assert_eq!(heard.narrated_turns, 0);
        assert_eq!(heard.spoken_us, Microseconds::ZERO);
        assert_eq!(heard.unnarrated_turns, vec!["turn-unread".to_string()]);
        assert!(heard.speakers.is_empty());
        assert_eq!(heard.gaps.count, 0);
    }

    #[test]
    fn planned_and_placed_score_are_counted_separately() {
        use crate::video::score::{CueAnchor, CueRole, MusicCue};

        let mut manifest = manifest();
        perform(&mut manifest, &[("narrator", 0, 5_000_000, true)]);
        for (id, placed) in [("cue-planned", false), ("cue-placed", true)] {
            manifest.music_cues.push(MusicCue {
                id: id.into(),
                role: CueRole::Sting,
                anchor: CueAnchor::Turn {
                    turn_id: "turn-0".into(),
                },
                target_duration_us: Microseconds(4_000_000),
                direction: "warm".into(),
                source_asset_id: placed.then(|| "music-one".to_string()),
                track_id: None,
                gain_db_milli: -6_000,
                fade_in_us: Microseconds(200_000),
                fade_out_us: Microseconds(200_000),
                created_at: NOW.into(),
            });
        }
        let heard = listen_to_episode(&manifest, None).unwrap();
        assert_eq!(heard.music_cues_planned, 1);
        assert_eq!(heard.music_cues_placed, 1);
    }
}
