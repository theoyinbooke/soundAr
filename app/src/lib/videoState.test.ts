import { describe, expect, it } from "vitest";
import { createBrowserPreviewVideoService } from "./videoBridge";
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
