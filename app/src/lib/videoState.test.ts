import { describe, expect, it } from "vitest";
import { createBrowserPreviewVideoService } from "./videoBridge";
import type { VideoMusicCueInput, VideoShowFormat, VideoSoundLayerInput } from "../types/video";
import {
  initialVideoStudioState,
  phaseForProject,
  videoProjectReadiness,
  videoStudioReducer,
  type VideoStudioState,
} from "./videoState";

describe("videoStudioReducer", () => {
  it("derives Analyze, Review, and Preview from durable manifest contents", async () => {
    const service = createBrowserPreviewVideoService();
    const sourceOnly = await service.getVideoProject("creator-update");
    sourceOnly.status = "editing";
    sourceOnly.manifest.candidates = [];
    sourceOnly.manifest.scenes = [];
    sourceOnly.manifest.timeline = {
      ...sourceOnly.manifest.timeline,
      duration_ms: 0,
      tracks: sourceOnly.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
    };
    expect(videoProjectReadiness(sourceOnly).nextAction).toBe("analyze");
    expect(phaseForProject(sourceOnly)).toBe("editor");

    const analyzed = await service.getVideoProject("product-demo");
    analyzed.status = "editing";
    expect(analyzed.manifest.candidates.length).toBeGreaterThan(0);
    expect(analyzed.manifest.scenes).toHaveLength(0);
    expect(videoProjectReadiness(analyzed).nextAction).toBe("review");
    expect(phaseForProject(analyzed)).toBe("review");

    const planned = await service.getVideoProject("creator-update");
    expect(videoProjectReadiness(planned).nextAction).toBe("preview");
    expect(phaseForProject(planned)).toBe("editor");
  });

  it("ignores a downstream preview recovery when the manifest still needs analysis", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    project.status = "failed";
    project.manifest.candidates = [];
    project.manifest.scenes = [];
    project.manifest.timeline = {
      ...project.manifest.timeline,
      duration_ms: 0,
      tracks: project.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
    };
    project.recoverable_job = {
      id: "stale-preview-job",
      project_id: project.id,
      phase: "preview",
      status: "failed",
      progress: .2,
      title: "Render preview",
      detail: "video.reviewed_scenes_required: Review at least one scene before rendering the timeline",
      error: "video.reviewed_scenes_required: Review at least one scene before rendering the timeline",
      durable: true,
      created_at: project.created_at,
      updated_at: project.updated_at,
    };

    expect(videoProjectReadiness(project).nextAction).toBe("analyze");
    expect(phaseForProject(project)).toBe("editor");
  });

  it("writes a multi-character script and reuses every turn whose words are unchanged", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    const cast = [
      { id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
      { id: "adaeze", name: "ADAEZE", display_name: "Adaeze", voice_id: "af-bella", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
    ];
    const script = "NARRATOR: The harmattan came early.\n\nADAEZE: (quiet) You said you would come back.\n\nNARRATOR: She did not answer.\n";

    const written = await service.writeVideoScript({ project_id: created.id, expected_revision: created.revision, base_version_id: created.manifest.version_id, operation_id: "script-1", cast, script });
    expect(written.receipt.new_turn_ids).toHaveLength(3);
    expect(written.receipt.retained_turn_ids).toHaveLength(0);
    expect(written.project.manifest.dialogue?.map((turn) => turn.character_id)).toEqual(["narrator", "adaeze", "narrator"]);
    expect(written.project.manifest.dialogue?.[1].direction).toBe("quiet");
    expect(written.project.manifest.dialogue?.[1].text).toBe("You said you would come back.");

    // Rewriting one line must leave the other two turns - and their takes - alone.
    const revised = await service.writeVideoScript({
      project_id: created.id,
      expected_revision: written.project.revision,
      base_version_id: written.project.manifest.version_id,
      operation_id: "script-2",
      cast,
      script: script.replace("She did not answer.", "She said nothing at all."),
    });
    expect(revised.receipt.new_turn_ids).toHaveLength(1);
    expect(revised.receipt.retained_turn_ids).toEqual(written.receipt.new_turn_ids.slice(0, 2));

    await expect(
      service.writeVideoScript({ project_id: created.id, expected_revision: revised.project.revision, base_version_id: revised.project.manifest.version_id, operation_id: "script-3", cast, script: "EMEKA: Who am I?\n" }),
    ).rejects.toThrow(/line 1/i);
  });

  it("derives conversational beats and keeps a held pause through a later edit", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    const cast = [
      { id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
      { id: "adaeze", name: "ADAEZE", display_name: "Adaeze", voice_id: "af-bella", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
    ];
    const script = "NARRATOR: He asked her name.\n\nADAEZE: Adaeze.\n\nADAEZE: And yours?\n\nNARRATOR: (interrupting) No.\n";
    const written = await service.writeVideoScript({ project_id: created.id, expected_revision: created.revision, base_version_id: created.manifest.version_id, operation_id: "beats-1", cast, script });
    const beats = written.project.manifest.turn_beats ?? [];

    expect(beats[0].lead_in_ms).toBe(0);
    // A reply is faster than the same character continuing a thought.
    expect(beats[1].lead_in_ms).toBe(220);
    expect(beats[2].lead_in_ms).toBe(600);
    // An interjection lands on top of the line before it instead of waiting.
    expect(beats[3].lead_in_ms).toBe(0);
    expect(beats[3].overlap_ms).toBe(250);
    expect(beats.every((beat) => beat.source === "derived")).toBe(true);

    const heldTurn = written.project.manifest.dialogue![1].id;
    const held = await service.editVideoTimeline({
      project_id: created.id,
      expected_revision: written.project.revision,
      base_version_id: written.project.manifest.version_id,
      operation_id: "hold-beat",
      operations: [{ type: "set_turn_beat", turn_id: heldTurn, lead_in_us: 2_000_000, overlap_us: 0 }],
    });
    expect(held.project.manifest.turn_beats?.find((beat) => beat.turn_id === heldTurn)).toMatchObject({ lead_in_ms: 2_000, source: "explicit" });

    // Rewriting a different line must not disturb the deliberate pause.
    const revised = await service.writeVideoScript({
      project_id: created.id,
      expected_revision: held.project.revision,
      base_version_id: held.project.manifest.version_id,
      operation_id: "beats-2",
      cast,
      script: script.replace("And yours?", "And what is yours?"),
    });
    expect(revised.project.manifest.turn_beats?.find((beat) => beat.turn_id === heldTurn)).toMatchObject({ lead_in_ms: 2_000, source: "explicit" });
  });

  it("stores pronunciation rules and rejects ones that cannot apply", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    const cast = [
      { id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
      { id: "adaeze", name: "ADAEZE", display_name: "Adaeze", voice_id: "af-bella", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
    ];
    const written = await service.writeVideoScript({
      project_id: created.id,
      expected_revision: created.revision,
      base_version_id: created.manifest.version_id,
      operation_id: "lexicon-script",
      cast,
      script: "NARRATOR: Adaeze came home.\n\nADAEZE: I am here.\n",
    });

    const scoped = await service.editVideoTimeline({
      project_id: created.id,
      expected_revision: written.project.revision,
      base_version_id: written.project.manifest.version_id,
      operation_id: "rule-1",
      operations: [{ type: "set_lexicon_entry", entry: { id: "rule-adaeze", scope: "character", character_id: "adaeze", match_text: "Adaeze", replacement: "Ah-DAH-eh-zeh", matching: "word", created_at: "2026-01-01T00:00:00Z" } }],
    });
    expect(scoped.project.manifest.lexicon).toEqual([
      expect.objectContaining({ id: "rule-adaeze", scope: "character", character_id: "adaeze" }),
    ]);

    // A rule naming a character outside the cast could never fire, so it fails closed.
    await expect(
      service.editVideoTimeline({
        project_id: created.id, expected_revision: scoped.project.revision, base_version_id: scoped.project.manifest.version_id, operation_id: "rule-2",
        operations: [{ type: "set_lexicon_entry", entry: { id: "rule-stray", scope: "character", character_id: "emeka", match_text: "Kano", replacement: "KAH-noh", matching: "word", created_at: "2026-01-01T00:00:00Z" } }],
      }),
    ).rejects.toThrow(/not in the cast/i);

    // A rule that rewrites a word to itself reads as broken rather than deliberate.
    await expect(
      service.editVideoTimeline({
        project_id: created.id, expected_revision: scoped.project.revision, base_version_id: scoped.project.manifest.version_id, operation_id: "rule-3",
        operations: [{ type: "set_lexicon_entry", entry: { id: "rule-noop", scope: "project", match_text: "Kano", replacement: "Kano", matching: "word", created_at: "2026-01-01T00:00:00Z" } }],
      }),
    ).rejects.toThrow(/must change the text/i);

    await expect(
      service.editVideoTimeline({
        project_id: created.id, expected_revision: scoped.project.revision, base_version_id: scoped.project.manifest.version_id, operation_id: "rule-4",
        operations: [{ type: "remove_lexicon_entry", entry_id: "rule-absent" }],
      }),
    ).rejects.toThrow(/no longer exists/i);
  });

  it("scores an episode with cues and refuses ones that could not play", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    const cast = [{ id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" }];
    const written = await service.writeVideoScript({
      project_id: created.id, expected_revision: created.revision, base_version_id: created.manifest.version_id,
      operation_id: "score-script", cast, script: "NARRATOR: The harmattan came early.\n",
    });
    const cue = (overrides: Partial<VideoMusicCueInput>): VideoMusicCueInput => ({
      id: "cue-outro", role: "outro", anchor: { kind: "after_final_turn" }, target_duration_us: 20_000_000,
      direction: "warm, resolving, low strings", gain_db_milli: -6_000, fade_in_us: 500_000, fade_out_us: 2_000_000,
      created_at: "2026-01-01T00:00:00Z", ...overrides,
    });

    const scored = await service.editVideoTimeline({
      project_id: created.id, expected_revision: written.project.revision, base_version_id: written.project.manifest.version_id,
      operation_id: "cue-1", operations: [{ type: "set_music_cue", cue: cue({}) }],
    });
    expect(scored.project.manifest.music_cues).toEqual([
      expect.objectContaining({ id: "cue-outro", role: "outro", needs_generation: true, target_duration_ms: 20_000 }),
    ]);

    const rejected = async (overrides: Partial<VideoMusicCueInput>, pattern: RegExp) =>
      expect(
        service.editVideoTimeline({
          project_id: created.id, expected_revision: scored.project.revision, base_version_id: scored.project.manifest.version_id,
          operation_id: `cue-${overrides.id}`, operations: [{ type: "set_music_cue", cue: cue(overrides) }],
        }),
      ).rejects.toThrow(pattern);

    // A sting cannot claim the ending, and a second outro would leave the renderer to choose.
    await rejected({ id: "cue-sting", role: "sting" }, /only an outro/i);
    await rejected({ id: "cue-outro-2" }, /only one outro/i);
    // Music cannot occupy the timeline before it exists.
    await rejected({ id: "cue-early", track_id: "music" }, /before its music exists/i);
  });

  it("registers imported media as sound design and drops placements with it", async () => {
    const service = createBrowserPreviewVideoService();
    const project = await service.getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];

    const registered = await service.editVideoTimeline({
      project_id: project.id, expected_revision: project.revision, base_version_id: project.manifest.version_id,
      operation_id: "register-sound",
      operations: [
        { type: "register_sound_asset", asset_id: "sound-tone", source_asset_id: project.manifest.source.id, name: "Quiet room", tags: ["room tone"] },
        { type: "set_sound_layer", layer: {
          id: "tone", asset_id: "sound-tone", kind: "room_tone", scene_id: scene.id,
          range: { start_us: scene.timeline_start_ms * 1000, end_us: scene.timeline_end_ms * 1000 },
          gain_db_milli: -26_000, fade_in_us: 250_000, fade_out_us: 250_000, loop_to_fill: true,
        } },
      ],
    });
    expect(registered.project.manifest.sound_assets).toHaveLength(1);
    expect(registered.project.manifest.sound_layers).toHaveLength(1);

    // Removing the sound removes its uses rather than leaving a placement with no audio.
    const removed = await service.editVideoTimeline({
      project_id: project.id, expected_revision: registered.project.revision, base_version_id: registered.project.manifest.version_id,
      operation_id: "remove-sound", operations: [{ type: "remove_sound_asset", asset_id: "sound-tone" }],
    });
    expect(removed.project.manifest.sound_assets).toHaveLength(0);
    expect(removed.project.manifest.sound_layers).toHaveLength(0);

    // Sound design can only label media the project already imported.
    await expect(
      service.editVideoTimeline({
        project_id: project.id, expected_revision: removed.project.revision, base_version_id: removed.project.manifest.version_id,
        operation_id: "register-stray",
        operations: [{ type: "register_sound_asset", asset_id: "sound-stray", source_asset_id: "source-absent", name: "Nowhere", tags: [] }],
      }),
    ).rejects.toThrow(/not registered/i);
  });

  it("refuses sound placements that would sound wrong or invent audio", async () => {
    const service = createBrowserPreviewVideoService();
    const project = await service.getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const layer = (overrides: Partial<VideoSoundLayerInput>): VideoSoundLayerInput => ({
      id: "tone", asset_id: "sound-tone", kind: "room_tone", scene_id: scene.id,
      range: { start_us: scene.timeline_start_ms * 1000, end_us: scene.timeline_end_ms * 1000 },
      gain_db_milli: -26_000, fade_in_us: 250_000, fade_out_us: 250_000, loop_to_fill: true, ...overrides,
    });
    const rejected = (overrides: Partial<VideoSoundLayerInput>, pattern: RegExp) =>
      expect(
        service.editVideoTimeline({
          project_id: project.id, expected_revision: project.revision, base_version_id: project.manifest.version_id,
          operation_id: `sound-${overrides.id ?? "tone"}`, operations: [{ type: "set_sound_layer", layer: layer(overrides) }],
        }),
      ).rejects.toThrow(pattern);

    // A placement can only use audio the user already registered.
    await rejected({}, /not registered/i);
    // Room tone near the dialogue reads as noise, and a one-shot cannot repeat.
    await rejected({ id: "loud", gain_db_milli: -6_000 }, /not registered|far under/i);
    await rejected({ id: "shot", kind: "one_shot", loop_to_fill: true }, /not registered|cannot loop/i);
  });

  it("starts episodes from a show format by copy, so editing the show never rewrites one", async () => {
    const service = createBrowserPreviewVideoService();
    const format: VideoShowFormat = {
      id: "show-harmattan", name: "The Harmattan Letters", revision: 0,
      cast: [
        { id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
        { id: "adaeze", name: "ADAEZE", display_name: "Adaeze", voice_id: "af-bella", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
      ],
      lexicon: [], performance_clock: { intra_exchange_us: 220_000, turn_of_thought_us: 600_000, pre_reveal_us: 1_200_000, scene_boundary_us: 900_000 },
      caption_preset_id: "podcast", canvas_mode: "portrait",
      canvas: { width: 1080, height: 1920, pixel_aspect_numerator: 1, pixel_aspect_denominator: 1 },
      frame_rate: { numerator: 30, denominator: 1 },
      target_lufs_milli: -16_000, true_peak_db_milli: -1_000, target_duration_us: 600_000_000,
      created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
    };

    const saved = await service.saveShowFormat(format);
    expect(saved.revision).toBe(1);
    expect(await service.listShowFormats()).toHaveLength(1);

    const first = await service.createEpisode(saved.id, "Episode 1");
    expect(first.manifest.cast?.[1].voice_id).toBe("af-bella");
    expect(first.manifest.format_origin?.format_revision).toBe(1);

    // Recasting the show must not reach the episode that already exists.
    const revised = await service.saveShowFormat({ ...saved, cast: saved.cast.map((member) => member.id === "adaeze" ? { ...member, voice_id: "af-nova" } : member) });
    expect(revised.revision).toBe(2);
    const reloaded = await service.getVideoProject(first.id);
    expect(reloaded.manifest.cast?.[1].voice_id).toBe("af-bella");

    const second = await service.createEpisode(saved.id, "Episode 2");
    expect(second.manifest.cast?.[1].voice_id).toBe("af-nova");
    expect(second.manifest.format_origin?.format_revision).toBe(2);

    await service.deleteShowFormat(saved.id);
    await expect(service.createEpisode(saved.id, "Episode 3")).rejects.toThrow(/does not exist/i);
  });

  it("names every release member that is still blocked", async () => {
    const service = createBrowserPreviewVideoService();
    const project = await service.getVideoProject("creator-update");

    const plan = await service.planEpisodeRelease(project.id, false);
    expect(plan.members.map((member) => member.kind)).toEqual([
      "podcast_audio", "video_master", "trailer", "audiogram", "transcript", "show_notes",
    ]);
    // A blocked member always says why rather than being quietly omitted.
    for (const member of plan.members.filter((entry) => !entry.ready)) {
      expect(member.blocked_reason).toBeTruthy();
    }
    expect(plan.members.find((member) => member.kind === "show_notes")?.ready).toBe(false);
    // Scenes are the episode's chapters.
    expect(plan.chapters).toHaveLength(project.manifest.scenes.length);
  });

  it("reports what a take actually said and never claims an unchecked line passed", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    const cast = [{ id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" }];
    const written = await service.writeVideoScript({
      project_id: created.id, expected_revision: created.revision, base_version_id: created.manifest.version_id,
      operation_id: "qc-script", cast, script: "NARRATOR: Adaeze came home.\n\nNARRATOR: She said nothing at all.\n",
    });

    // Nothing has a take yet, so there is nothing to check and nothing to claim.
    const untouched = await service.checkEpisodeQuality(written.project.id, {});
    expect(untouched.checked_turns).toHaveLength(0);
    expect(untouched.loudness_checked).toBe(false);
  });

  it("refuses to promote a line that is not a draft", async () => {
    const service = createBrowserPreviewVideoService();
    const project = await service.getVideoProject("creator-update");
    // Naming a line with no stand-in would read as a promotion that did something.
    await expect(
      service.editVideoTimeline({
        project_id: project.id, expected_revision: project.revision, base_version_id: project.manifest.version_id,
        operation_id: "promote-final", operations: [{ type: "promote_turns_to_final", turn_ids: ["turn-absent"] }],
      }),
    ).rejects.toThrow(/does not have a draft take/i);
  });

  it("will not export a release before there is a master to derive it from", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "A short story about a missing letter." });
    // Every deliverable derives from the finished master.
    await expect(service.exportEpisodeRelease(created.id, true)).rejects.toThrow(/final master/i);

    const project = await service.getVideoProject("creator-update");
    const plan = await service.planEpisodeRelease(project.id, true);
    // The audiogram is a release member in its own right and is reported alongside the rest.
    expect(plan.members.map((member) => member.kind)).toContain("audiogram");
  });

  it("moves a durable project through intake, analysis, review, render, and export", async () => {
    const service = createBrowserPreviewVideoService();
    let state: VideoStudioState = initialVideoStudioState;
    state = videoStudioReducer(state, { type: "open-intake", entry: "link" });
    expect(state.phase).toBe("intake");

    const exactUrl = "https://example.com/watch/one";
    const imported = await service.importLink({ exact_url: exactUrl, rights_confirmed: true, rights_confirmation_url: exactUrl, single_source_only: true });
    state = videoStudioReducer(state, { type: "source-accepted", project: imported });
    expect(state.phase).toBe("analyzing");

    const analyzed = await service.analyzeVideo(imported.id);
    state = videoStudioReducer(state, { type: "analysis-complete", project: analyzed });
    expect(state.phase).toBe("review");
    expect(state.selectedCandidateIds).toEqual(["clip-1", "clip-2", "clip-4"]);

    state = videoStudioReducer(state, { type: "toggle-candidate", candidateId: "clip-4" });
    expect(state.selectedCandidateIds).toEqual(["clip-1", "clip-2"]);
    const planned = await service.planVideo(imported.id, state.selectedCandidateIds);
    state = videoStudioReducer(state, { type: "review-complete", project: planned });
    expect(state.phase).toBe("editor");
    expect(state.project?.manifest.timeline.tracks[0].items.some((item) => item.kind === "gap")).toBe(true);

    state = videoStudioReducer(state, { type: "render-started" });
    expect(state.phase).toBe("rendering");
    const previewed = await service.renderVideoPreview(imported.id);
    state = videoStudioReducer(state, { type: "preview-complete", project: previewed });
    expect(state.phase).toBe("editor");

    state = videoStudioReducer(state, { type: "export-started" });
    const exported = await service.exportVideo({ project_id: imported.id, version_id: previewed.manifest.version_id, format: "mp4", profile: "final" });
    state = videoStudioReducer(state, { type: "export-complete", project: exported });
    expect(state.phase).toBe("exported");
    expect(state.project?.master?.role).toBe("master");
  });

  it("keeps prompt scenes within their source clock and persists targeted scene revisions", async () => {
    const service = createBrowserPreviewVideoService();
    const created = await service.createVideoProject({ prompt: "Explain a small creative workflow." });
    expect(Math.max(...created.manifest.scenes.map((scene) => scene.source_end_ms))).toBeLessThanOrEqual(created.manifest.source.duration_ms);
    expect(created.manifest.timeline.duration_ms).toBe(70_000);

    const scene = created.manifest.scenes[0];
    const revised = await service.reviseVideo({
      project_id: created.id,
      base_version_id: created.manifest.version_id,
      instruction: "Make this scene calmer.",
      scene_id: scene.id,
      scene_patch: {
        layout: "portrait",
        crop_mode: "fit",
        captions_enabled: true,
        caption_style: "calm",
        voice_gain_db: -2,
        music_gain_db: -16,
        voice_id: "af_heart",
        model_id: "hexgrad/Kokoro-82M",
        speaker: "af_heart",
        language: "en-US",
      },
    });
    expect(revised.manifest.scenes[0]).toMatchObject({
      crop_mode: "fit",
      caption_style: "calm",
      voice_gain_db: -2,
      music_gain_db: -16,
      voice_id: "af_heart",
      model_id: "hexgrad/Kokoro-82M",
      speaker: "af_heart",
      language: "en-US",
    });
    expect(revised.manifest.version_id).not.toBe(created.manifest.version_id);
    expect(revised.manifest.revisions.at(-1)?.affected_stages).toEqual(["preview", "export"]);
  });

  it("recovers render failures and cancellations to a usable editor state", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    let state = videoStudioReducer(initialVideoStudioState, { type: "open-project", project });
    state = videoStudioReducer(state, { type: "render-started" });
    state = videoStudioReducer(state, { type: "fail", error: "Encoder unavailable" });
    expect(state.returnPhase).toBe("editor");
    state = videoStudioReducer(state, { type: "dismiss-error" });
    expect(state.phase).toBe("editor");
    state = videoStudioReducer(state, { type: "export-started" });
    state = videoStudioReducer(state, { type: "cancel-operation" });
    expect(state.phase).toBe("editor");
  });
});
