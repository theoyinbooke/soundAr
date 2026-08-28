import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VideoProject } from "../types/video";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  convertFileSrc: vi.fn((path: string) => `asset://${path}`),
}));

vi.mock("@tauri-apps/api/core", () => tauri);
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import { createVideoStudioService } from "./videoBridge";

describe("native Video Studio bridge", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
    tauri.invoke.mockReset();
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
});
