import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserPreviewVideoService } from "../../lib/videoBridge";
import { millisecondsToMicroseconds, VideoTimeline } from "./VideoTimeline";

afterEach(cleanup);

describe("VideoTimeline", () => {
  it("supports precise keyboard playhead movement and exposes preserved gaps", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const onPlayheadChange = vi.fn();
    const onSelectScene = vi.fn();
    render(<VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={10_000} selectedSceneId={project.manifest.scenes[0].id} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} />);

    const playhead = screen.getByRole("slider", { name: "Timeline playhead" });
    fireEvent.keyDown(playhead, { key: "ArrowRight" });
    expect(onPlayheadChange).toHaveBeenLastCalledWith(11_000);
    fireEvent.keyDown(playhead, { key: "ArrowLeft", shiftKey: true });
    expect(onPlayheadChange).toHaveBeenLastCalledWith(5_000);
    fireEvent.keyDown(playhead, { key: "End" });
    expect(onPlayheadChange).toHaveBeenLastCalledWith(project.manifest.timeline.duration_ms);

    expect(screen.getAllByRole("note", { name: /preserved silent gap/i }).length).toBeGreaterThan(0);
    await userEvent.click(screen.getAllByRole("button", { name: /Hook: where I’ve been.*source/i })[0]);
    expect(onSelectScene).toHaveBeenCalledWith("scene-clip-1");
  });

  it("keeps four selectable lanes visible and resizes with pointer-equivalent keyboard controls", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const timeline = {
      ...project.manifest.timeline,
      tracks: project.manifest.timeline.tracks.filter((track) => track.kind === "voice" || track.kind === "captions"),
    };
    const onHeightChange = vi.fn();
    render(<VideoTimeline timeline={timeline} scenes={project.manifest.scenes} playheadMs={0} onPlayheadChange={vi.fn()} onSelectScene={vi.fn()} height={250} onHeightChange={onHeightChange} />);

    expect(screen.getByRole("region", { name: "Video timeline" })).toHaveAttribute("data-track-count", "4");
    expect(screen.getByRole("group", { name: "Video track" })).toBeVisible();
    expect(screen.getByRole("group", { name: "Music track" })).toBeVisible();
    expect(screen.getAllByText(/No (video|music) assets/)).toHaveLength(2);

    await userEvent.click(screen.getByRole("button", { name: "Music" }));
    expect(screen.getByRole("button", { name: "Music" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(screen.getByRole("separator", { name: "Resize timeline" }), { key: "ArrowUp" });
    expect(onHeightChange).toHaveBeenLastCalledWith(270);
  });

  it("adds one compact selectable Visuals lane only when a durable layer exists", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const timeline = {
      ...project.manifest.timeline,
      tracks: [
        project.manifest.timeline.tracks[0],
        { kind: "visuals" as const, items: [{ id: "visual-layer-1", track: "visuals" as const, kind: "clip" as const, start_ms: scene.timeline_start_ms, end_ms: scene.timeline_end_ms, label: "Imported image", scene_id: scene.id, asset_id: "visual-1" }] },
        ...project.manifest.timeline.tracks.slice(1),
      ],
    };
    render(<VideoTimeline timeline={timeline} scenes={project.manifest.scenes} playheadMs={0} selectedSceneId={scene.id} onPlayheadChange={vi.fn()} onSelectScene={vi.fn()} mode="compact" height={210} />);

    const region = screen.getByRole("region", { name: "Video timeline" });
    expect(region).toHaveAttribute("data-track-count", "5");
    expect(screen.getByRole("group", { name: "Visuals track" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Visuals" }));
    expect(screen.getByRole("button", { name: "Visuals" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /Imported image, project/ })).toHaveTextContent("Imported image");
  });

  it("exposes three accessible timeline sizes and keyboard-switches between them", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const onModeChange = vi.fn();
    render(<VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={0} onPlayheadChange={vi.fn()} onSelectScene={vi.fn()} height={210} onHeightChange={vi.fn()} mode="compact" onModeChange={onModeChange} />);

    const timeline = screen.getByRole("region", { name: "Video timeline" });
    expect(timeline).toHaveAttribute("data-timeline-mode", "compact");
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Timeline size" }), "collapsed");
    expect(onModeChange).toHaveBeenLastCalledWith("collapsed");
    fireEvent.keyDown(screen.getByRole("separator", { name: "Resize timeline" }), { key: "ArrowUp" });
    expect(onModeChange).toHaveBeenLastCalledWith("expanded");
    fireEvent.keyDown(screen.getByRole("separator", { name: "Resize timeline" }), { key: "Home" });
    expect(onModeChange).toHaveBeenLastCalledWith("collapsed");
  });

  it("commits split, keyboard reorder, and keyboard trim as exact service operations", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const onEditTimeline = vi.fn(async () => undefined);
    render(<VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={10_000} selectedSceneId={scene.id} onPlayheadChange={vi.fn()} onSelectScene={vi.fn()} onEditTimeline={onEditTimeline} />);

    await userEvent.click(screen.getByRole("button", { name: "Split selected scene" }));
    expect(onEditTimeline).toHaveBeenLastCalledWith(
      [{ type: "split_scene", scene_id: scene.id, at_timeline_us: 10_000_000 }],
      "Split scene",
    );

    const clip = screen.getAllByRole("button", { name: /Hook: where I’ve been, project 00:00 to 00:18/ })[0];
    fireEvent.keyDown(clip, { key: "ArrowRight", altKey: true });
    expect(onEditTimeline).toHaveBeenLastCalledWith(
      [{ type: "reorder_scene", scene_id: scene.id, to_index: 1 }],
      "Move Hook: where I’ve been",
    );

    fireEvent.keyDown(screen.getByRole("separator", { name: "Trim start of Hook: where I’ve been" }), { key: "ArrowRight" });
    expect(onEditTimeline).toHaveBeenLastCalledWith(
      [{ type: "trim_scene", scene_id: scene.id, source_start_us: 14_100_000, source_end_us: 32_000_000 }],
      "Trim Hook: where I’ve been",
    );
  });

  it("commits a pointer reorder exactly once on pointer-up", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    const onEditTimeline = vi.fn(async () => undefined);
    render(<VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={0} selectedSceneId={scene.id} onPlayheadChange={vi.fn()} onSelectScene={vi.fn()} onEditTimeline={onEditTimeline} />);
    const clip = screen.getAllByRole("button", { name: /Hook: where I’ve been, project 00:00 to 00:18/ })[0];
    const lane = clip.parentElement?.parentElement;
    expect(lane).toBeTruthy();
    vi.spyOn(lane!, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, left: 0, top: 0, right: 1_000, bottom: 30, width: 1_000, height: 30, toJSON: () => ({}) });

    fireEvent.pointerDown(clip, { button: 0, clientX: 100 });
    fireEvent.pointerMove(window, { clientX: 900 });
    expect(onEditTimeline).not.toHaveBeenCalled();
    fireEvent.pointerUp(window, { clientX: 900 });

    expect(onEditTimeline).toHaveBeenCalledOnce();
    expect(onEditTimeline).toHaveBeenCalledWith([{ type: "reorder_scene", scene_id: scene.id, to_index: 2 }], "Move Hook: where I’ve been");
  });

  it("rejects timeline values outside JavaScript's exact microsecond range", () => {
    expect(millisecondsToMicroseconds(12_345.678)).toBe(12_345_678);
    expect(() => millisecondsToMicroseconds(Number.MAX_SAFE_INTEGER)).toThrow(/exact microsecond range/i);
  });
});
