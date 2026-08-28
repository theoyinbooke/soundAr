import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserPreviewVideoService } from "../../lib/videoBridge";
import { mapSourceToTimeline, mapTimelineToSource, projectVisualLayers, VideoPreviewPlayer } from "./VideoPreviewPlayer";

afterEach(cleanup);

describe("VideoPreviewPlayer", () => {
  it("maps project playhead time back to the selected scene source clock", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    render(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms} onPlayheadChange={vi.fn()} />);

    const video = screen.getByLabelText("Project proxy preview") as HTMLVideoElement;
    await waitFor(() => expect(video.currentTime).toBe(scene.source_start_ms / 1000));
  });

  it("shows only the active bounded transcript cue and never double-overlays a rendered preview", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = { ...project.manifest.scenes[0], transcript: "This entire scene transcript must never be dumped across the portrait canvas because it is far too long." };
    const transcript = [
      { id: "cue-1", start_ms: scene.source_start_ms, end_ms: scene.source_start_ms + 4_000, text: "First compact cue stays readable on screen", source_clock: true as const },
      { id: "cue-2", start_ms: scene.source_start_ms + 4_000, end_ms: scene.source_end_ms, text: "Second compact cue follows the playhead", source_clock: true as const },
    ];
    const { rerender } = render(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} transcript={transcript} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms} onPlayheadChange={vi.fn()} />);

    expect(screen.getByRole("button", { name: /Select active caption: First compact cue/ })).toBeVisible();
    expect(screen.queryByText(scene.transcript)).not.toBeInTheDocument();

    rerender(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} transcript={transcript} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms + 8_000} onPlayheadChange={vi.fn()} />);
    expect(screen.getByRole("button", { name: /Select active caption: Second compact cue/ })).toBeVisible();

    const artifact = project.manifest.artifacts.find((candidate) => candidate.role === "proxy")!;
    rerender(<VideoPreviewPlayer artifact={{ ...artifact, role: "preview" }} scene={scene} transcript={transcript} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms} onPlayheadChange={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /Select active caption/ })).not.toBeInTheDocument();
  });

  it("uses the renderer-authored caption page, timing, and preset without repaging it", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const page = {
      id: "authoritative-page", cue_id: "cue", scene_id: scene.id,
      start_ms: scene.timeline_start_ms, end_ms: scene.timeline_start_ms + 3_000,
      text: "Exactly as the renderer paged this cue", style_id: "bold-pop" as const,
      words: [{ text: "Exactly", start_ms: scene.timeline_start_ms, end_ms: scene.timeline_start_ms + 500 }],
    };
    const onCaptionBoundsChange = vi.fn();
    render(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} transcript={[{ id: "fallback", start_ms: scene.source_start_ms, end_ms: scene.source_end_ms, text: "This fallback must not appear", source_clock: true }]} captionPages={[page]} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms + 1_000} onPlayheadChange={vi.fn()} onCaptionBoundsChange={onCaptionBoundsChange} />);

    const caption = screen.getByRole("button", { name: `Select active caption: ${page.text}` });
    expect(caption).toHaveAttribute("data-caption-page-id", page.id);
    expect(caption).toHaveClass("is-bold-pop");
    expect(screen.queryByText(/fallback must not appear/i)).not.toBeInTheDocument();
    fireEvent.click(caption);
    expect(caption).toHaveAttribute("aria-pressed", "true");
    expect(caption.querySelectorAll(".video-caption-selection-handles i")).toHaveLength(4);
    fireEvent.keyDown(caption, { key: "ArrowRight" });
    expect(onCaptionBoundsChange).toHaveBeenLastCalledWith(expect.objectContaining({ x_bp: 900 }));

    const frame = caption.closest(".video-portrait-frame")!;
    vi.spyOn(frame, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, left: 0, top: 0, right: 200, bottom: 400, width: 200, height: 400, toJSON: () => ({}) });
    fireEvent.pointerDown(caption, { clientX: 50, clientY: 300 });
    fireEvent.pointerMove(window, { clientX: 70, clientY: 260 });
    fireEvent.pointerUp(window);
    expect(onCaptionBoundsChange).toHaveBeenLastCalledWith({ x_bp: 1_600, y_bp: 6_350, width_bp: 8_400, height_bp: 1_500 });
  });

  it("maps two cut scenes through a materialized editorial gap on the single project clock", () => {
    const first = {
      id: "first", position: 1, title: "First", source_start_ms: 5_000, source_end_ms: 7_000,
      timeline_start_ms: 0, timeline_end_ms: 2_000, transcript: "First cue", layout: "portrait" as const,
      crop_mode: "fit" as const, captions_enabled: true, caption_style: "clean-white" as const,
      voice_gain_db: 0, music_gain_db: -12,
    };
    const second = {
      ...first, id: "second", position: 2, title: "Second", source_start_ms: 12_000, source_end_ms: 14_000,
      timeline_start_ms: 3_000, timeline_end_ms: 5_000, transcript: "Second cue",
    };

    expect(mapTimelineToSource([first, second], 1_250, 5_000)).toMatchObject({ kind: "scene", scene: { id: "first" }, sourceMs: 6_250 });
    expect(mapTimelineToSource([first, second], 2_500, 5_000)).toMatchObject({ kind: "gap", startMs: 2_000, endMs: 3_000, previousScene: { id: "first" }, nextScene: { id: "second" } });
    expect(mapTimelineToSource([first, second], 3_500, 5_000)).toMatchObject({ kind: "scene", scene: { id: "second" }, sourceMs: 12_500 });
  });

  it("maps 2x and 0.5x scenes proportionally in both timeline directions", () => {
    const base = {
      id: "rate", position: 1, title: "Rate", source_start_ms: 10_000, source_end_ms: 14_000,
      timeline_start_ms: 2_000, timeline_end_ms: 4_000, transcript: "Rate cue", layout: "portrait" as const,
      crop_mode: "fit" as const, captions_enabled: true, caption_style: "clean-white" as const,
      voice_gain_db: 0, music_gain_db: -12,
    };
    expect(mapTimelineToSource([base], 3_000, 4_000)).toMatchObject({ kind: "scene", sourceMs: 12_000 });
    expect(mapSourceToTimeline(base, 13_000)).toBe(3_500);

    const halfSpeed = { ...base, source_end_ms: 12_000, timeline_end_ms: 6_000 };
    expect(mapTimelineToSource([halfSpeed], 4_000, 6_000)).toMatchObject({ kind: "scene", sourceMs: 11_000 });
    expect(mapSourceToTimeline(halfSpeed, 11_500)).toBe(5_000);
  });

  it("projects saved visual motion and fades on the source preview without double-overlaying a render", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const asset = {
      id: "visual-1", mime_type: "image/webp" as const, local_path: "/managed/visual-1.webp", url: "/video-studio-editorial-visual.webp",
      width: 900, height: 1_600, has_alpha: false, size_bytes: 42_000, checksum: "a".repeat(64),
      provenance: { kind: "user_upload" as const, imported_at: project.created_at, producer: "soundAr Video Studio", metadata: {} }, created_at: project.created_at,
    };
    const layer = {
      id: "layer-1", asset_id: asset.id, scene_id: scene.id, start_ms: 0, end_ms: 10_000, fit: "contain" as const, z_index: 10,
      motion: {
        start_bounds: { x_bp: 0, y_bp: 0, width_bp: 5_000, height_bp: 5_000 },
        end_bounds: { x_bp: 5_000, y_bp: 5_000, width_bp: 5_000, height_bp: 5_000 },
        start_opacity_milli: 1_000, end_opacity_milli: 1_000,
        start_rotation_milli_degrees: 0, end_rotation_milli_degrees: 0, easing: "linear" as const,
      },
      transition_in_ms: 1_000, transition_out_ms: 1_000,
    };
    expect(projectVisualLayers([asset], [layer], 5_000)[0]).toMatchObject({ bounds: { x_bp: 2_500, y_bp: 2_500, width_bp: 5_000, height_bp: 5_000 }, opacity: 1 });
    expect(projectVisualLayers([asset], [layer], 500)[0].opacity).toBeCloseTo(.5);

    const { rerender } = render(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} scenes={project.manifest.scenes} visualAssets={[asset]} visualLayers={[layer]} projectDurationMs={project.duration_ms} playheadMs={5_000} onPlayheadChange={vi.fn()} />);
    expect(document.querySelector('[data-visual-layer-id="layer-1"]')).toBeVisible();

    const artifact = { ...project.manifest.artifacts[0], role: "preview" as const };
    rerender(<VideoPreviewPlayer artifact={artifact} scene={scene} scenes={project.manifest.scenes} visualAssets={[asset]} visualLayers={[layer]} projectDurationMs={project.duration_ms} playheadMs={5_000} onPlayheadChange={vi.fn()} />);
    expect(document.querySelector('[data-visual-layer-id="layer-1"]')).not.toBeInTheDocument();
  });
});
