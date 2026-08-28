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
