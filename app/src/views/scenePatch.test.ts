import { describe, expect, it } from "vitest";
import { changedScenePatch } from "./VideoStudioView";
import type { VideoScene } from "../types/video";

function scene(overrides: Partial<VideoScene> = {}): VideoScene {
  return {
    id: "scene-1",
    position: 1,
    title: "Opening",
    transcript: "Every story starts as a small idea.",
    source_start_ms: 0,
    source_end_ms: 15_200,
    timeline_start_ms: 0,
    timeline_end_ms: 15_200,
    layout: "portrait",
    crop_mode: "auto-center",
    captions_enabled: true,
    caption_style: "calm",
    caption_bounds: { x_bp: 800, y_bp: 7350, width_bp: 8400, height_bp: 1500 },
    voice_gain_db: 0,
    music_gain_db: -12,
    voice_id: "af_heart",
    model_id: "hexgrad/Kokoro-82M",
    speaker: "af_heart",
    language: "en-US",
    ...overrides,
  } as VideoScene;
}

describe("scene save patch", () => {
  it("sends nothing when the scene is unchanged", () => {
    const saved = scene();
    // Replaying settled fields made the backend re-apply a crop that a narration-plus-images
    // project has no clip for, which failed the save outright.
    expect(changedScenePatch(saved, { ...saved })).toEqual({});
  });

  it("sends only the field the editor actually changed", () => {
    const saved = scene();
    expect(changedScenePatch(saved, scene({ caption_style: "bold-pop" }))).toEqual({ caption_style: "bold-pop" });
    expect(changedScenePatch(saved, scene({ voice_gain_db: -3 }))).toEqual({ voice_gain_db: -3 });
  });

  it("compares caption bounds by value, not identity", () => {
    const saved = scene();
    const sameBounds = scene({ caption_bounds: { ...saved.caption_bounds! } });
    expect(changedScenePatch(saved, sameBounds)).toEqual({});
    const moved = scene({ caption_bounds: { ...saved.caption_bounds!, x_bp: 1_200 } });
    expect(changedScenePatch(saved, moved)).toHaveProperty("caption_bounds.x_bp", 1_200);
  });

  it("carries the crop rectangle only for manual framing", () => {
    const saved = scene();
    const manual = scene({ crop_mode: "manual", crop_rect: { x_bp: 0, y_bp: 0, width_bp: 5_000, height_bp: 5_000 } });
    expect(changedScenePatch(saved, manual)).toEqual({ crop_mode: "manual", crop_rect: manual.crop_rect });
    // Leaving manual mode drops the rectangle, which the backend rejects outside manual framing.
    expect(changedScenePatch(manual, scene({ crop_mode: "fit" }))).toEqual({ crop_mode: "fit" });
  });

  it("resends a manual crop when only its rectangle moved", () => {
    const manual = scene({ crop_mode: "manual", crop_rect: { x_bp: 0, y_bp: 0, width_bp: 5_000, height_bp: 5_000 } });
    const nudged = scene({ crop_mode: "manual", crop_rect: { x_bp: 500, y_bp: 0, width_bp: 5_000, height_bp: 5_000 } });
    expect(changedScenePatch(manual, nudged)).toEqual({ crop_mode: "manual", crop_rect: nudged.crop_rect });
  });

  it("sends the narration route as a complete selection when any part changes", () => {
    const saved = scene();
    // A partial route is not a valid selection, so the four fields move together.
    expect(changedScenePatch(saved, scene({ voice_id: "am_adam" }))).toEqual({
      voice_id: "am_adam",
      model_id: saved.model_id,
      speaker: saved.speaker,
      language: saved.language,
    });
  });

  it("sends every field for a scene the saved version does not know", () => {
    const patch = changedScenePatch(undefined, scene());
    expect(Object.keys(patch).sort()).toEqual([
      "caption_bounds", "caption_style", "captions_enabled", "crop_mode",
      "language", "layout", "model_id", "music_gain_db", "speaker", "voice_gain_db", "voice_id",
    ]);
  });
});
