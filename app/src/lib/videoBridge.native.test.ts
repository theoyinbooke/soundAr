import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VideoProject } from "../types/video";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  convertFileSrc: vi.fn((path: string) => `asset://${path}`),
  open: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => tauri);
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: tauri.open }));

import { createVideoStudioService } from "./videoBridge";

describe("native Video Studio bridge", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
    tauri.invoke.mockReset();
    tauri.open.mockReset();
  });

  afterEach(() => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("forwards the complete narration route without renaming or stripping fields", async () => {
    tauri.invoke.mockResolvedValue({
      id: "project-1",
      manifest: { source: {}, artifacts: [], narration_bindings: [] },
    } as unknown as VideoProject);
    const request = {
      project_id: "project-1",
      base_version_id: "version-4",
      instruction: "Change the voice",
      scene_id: "scene-opening",
      scene_patch: {
        voice_id: "af_heart",
        model_id: "hexgrad/Kokoro-82M",
        speaker: "af_heart",
        language: "en-US",
      },
    };

    await createVideoStudioService().reviseVideo(request);

    expect(tauri.invoke).toHaveBeenCalledWith("revise_video", { request });
    expect(tauri.invoke.mock.calls[0][1]).toMatchObject({
      request: {
        scene_patch: {
          voice_id: "af_heart",
          model_id: "hexgrad/Kokoro-82M",
          speaker: "af_heart",
          language: "en-US",
        },
      },
    });
  });

  it("forwards exact revision-bound microsecond timeline edits and projects the returned project", async () => {
    const project = {
      id: "project-1",
      revision: 8,
      manifest: { version_id: "version-9", source: {}, artifacts: [], narration_bindings: [] },
    } as unknown as VideoProject;
    tauri.invoke.mockResolvedValue({
      project,
      receipt: {
        project_id: "project-1",
        expected_revision: 7,
        base_version_id: "version-8",
        operation_id: "timeline-operation-1",
        changed_paths: ["reviewed_scenes"],
        invalidated_stages: ["preview"],
      },
      job_id: "timeline-job-1",
      replayed: false,
    });
    const request = {
      project_id: "project-1",
      expected_revision: 7,
      base_version_id: "version-8",
      operation_id: "timeline-operation-1",
      operations: [{ type: "split_scene" as const, scene_id: "scene-opening", at_timeline_us: 12_345_000 }],
    };

    const response = await createVideoStudioService().editVideoTimeline(request);

    expect(tauri.invoke).toHaveBeenCalledWith("edit_video_timeline", { request });
    expect(response).toMatchObject({ project: { revision: 8 }, job_id: "timeline-job-1", replayed: false });
  });

  it("confirms the exact chosen image and forwards the strict visual composition request", async () => {
    tauri.open.mockResolvedValue("/home/user/Pictures/cover.webp");
    const project = {
      id: "project-1",
      revision: 5,
      manifest: {
        version_id: "version-6",
        source: {},
        artifacts: [],
        narration_bindings: [],
        visual_assets: [{
          id: "visual-1",
          mime_type: "image/webp",
          local_path: "/managed/project-1/visual-1.webp",
          width: 1080,
          height: 1920,
          has_alpha: false,
          size_bytes: 42_000,
          checksum: "a".repeat(64),
          provenance: { kind: "user_upload", imported_at: "2026-08-28T00:00:00Z", producer: "soundAr Video Studio", metadata: {} },
          created_at: "2026-08-28T00:00:00Z",
        }],
        visual_layers: [],
      },
    } as unknown as VideoProject;
    tauri.invoke.mockResolvedValue({ project, asset_id: "visual-1", layer_id: "layer-1", job_id: "visual-job-1", replayed: false });
    const service = createVideoStudioService();

    const selection = await service.pickLocalVisual?.();
    expect(tauri.open).toHaveBeenCalledWith({ multiple: false, directory: false, filters: [{ name: "Image", extensions: ["png", "jpg", "jpeg", "webp"] }] });
    expect(selection).toEqual({ local_path: "/home/user/Pictures/cover.webp", display_name: "cover.webp" });
    const request = {
      project_id: "project-1",
      expected_revision: 4,
      expected_version_id: "version-5",
      operation_id: "visual-operation-1",
      source_path: selection!.local_path,
      actor: "desktop-ui",
      origin: { kind: "user_selected" as const, user_confirmed: true as const },
      scene_id: "scene-opening",
      range: { start_us: 0, end_us: 12_000_000 },
      fit: "contain" as const,
      z_index: 10,
      motion: {
        start_bounds: { x_bp: 0, y_bp: 0, width_bp: 10_000, height_bp: 10_000 },
        end_bounds: { x_bp: 0, y_bp: 0, width_bp: 10_000, height_bp: 10_000 },
        start_opacity_milli: 1_000,
        end_opacity_milli: 1_000,
        start_rotation_milli_degrees: 0,
        end_rotation_milli_degrees: 0,
        easing: "ease_in_out" as const,
      },
      transition_in_us: 300_000,
      transition_out_us: 300_000,
    };

    const response = await service.addVideoVisualAsset(request);

    expect(tauri.invoke).toHaveBeenCalledWith("add_video_visual_asset", { request });
    expect(response.project.manifest.visual_assets?.[0].url).toBe("asset:///managed/project-1/visual-1.webp");
    expect(response).toMatchObject({ asset_id: "visual-1", layer_id: "layer-1", job_id: "visual-job-1", replayed: false });
  });
});
