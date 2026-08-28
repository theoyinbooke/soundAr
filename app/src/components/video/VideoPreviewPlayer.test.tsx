import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserPreviewVideoService } from "../../lib/videoBridge";
import { VideoPreviewPlayer } from "./VideoPreviewPlayer";

afterEach(cleanup);

describe("VideoPreviewPlayer", () => {
  it("maps project playhead time back to the selected scene source clock", async () => {
    const project = await createBrowserPreviewVideoService().getVideoProject("creator-update");
    const scene = project.manifest.scenes[0];
    render(<VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} scene={scene} projectDurationMs={project.duration_ms} playheadMs={scene.timeline_start_ms} onPlayheadChange={vi.fn()} />);

    const video = screen.getByLabelText("Project proxy preview") as HTMLVideoElement;
    await waitFor(() => expect(video.currentTime).toBe(scene.source_start_ms / 1000));
  });
});
