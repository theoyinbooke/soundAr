import { describe, expect, it } from "vitest";
import { createBrowserPreviewVideoService } from "./videoBridge";
import { initialVideoStudioState, videoStudioReducer, type VideoStudioState } from "./videoState";

describe("videoStudioReducer", () => {
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
