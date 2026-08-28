import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserPreviewVideoService } from "../../lib/videoBridge";
import { VideoTimeline } from "./VideoTimeline";

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
});
