import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VideoArtifact, VideoProjectSummary, VideoStudioService } from "../../types/video";
import { VideoIntegrationProvider } from "./VideoIntegrationContext";
import { VideoMasterCard } from "./VideoMasterCard";

afterEach(cleanup);

function artifact(id: string, role: VideoArtifact["role"], title: string, url: string): VideoArtifact {
  return {
    id,
    project_id: "project-1",
    version_id: "version-1",
    role,
    title,
    mime_type: role === "publish-package" ? "application/zip" : "video/mp4",
    format: role === "publish-package" ? "zip" : "mp4",
    url,
    local_path: `/exports/${id}.${role === "publish-package" ? "zip" : "mp4"}`,
    download_name: `${id}.${role === "publish-package" ? "zip" : "mp4"}`,
    duration_ms: role === "publish-package" ? undefined : 4_000,
    width: role === "publish-package" ? undefined : 1080,
    height: role === "publish-package" ? undefined : 1920,
    codec: role === "publish-package" ? undefined : "H.264",
    playable: role !== "publish-package",
    created_at: "2026-08-27T20:00:00Z",
  };
}

describe("VideoMasterCard", () => {
  it("keeps the primary master dominant while every final deliverable remains playable or downloadable", async () => {
    const master = artifact("master", "master", "Portrait master", "/master.mp4");
    const variation = artifact("variation", "variation", "Calm variation", "/variation.mp4");
    const publish = artifact("publish", "publish-package", "Publish package", "/publish.zip");
    const project: VideoProjectSummary = {
      id: "project-1",
      name: "Creator update",
      status: "exported",
      revision: 3,
      duration_ms: 4_000,
      scene_count: 2,
      updated_at: "2026-08-27T20:00:00Z",
      master,
      deliverables: [master, variation, publish],
    };
    const onOpen = vi.fn();
    const saveArtifact = vi.fn().mockResolvedValue("/home/creator/Videos/master.mp4");
    render(
      <VideoIntegrationProvider service={{ saveArtifact } as unknown as VideoStudioService} onOpenProject={onOpen}>
        <VideoMasterCard project={project} variant="history" onOpen={onOpen} />
      </VideoIntegrationProvider>,
    );

    expect(screen.getByLabelText("Play Portrait master")).toBeInstanceOf(HTMLVideoElement);
    expect(screen.getByLabelText("Play Portrait master")).toHaveAttribute("src", "/master.mp4#t=0.001");
    const details = screen.getByText("2 additional deliverables").closest("details");
    expect(details).not.toHaveAttribute("open");
    expect(screen.queryByLabelText("Play Calm variation")).not.toBeInTheDocument();
    await userEvent.click(within(details!).getByText("2 additional deliverables"));
    expect(within(details!).getByLabelText("Play Calm variation")).toBeInstanceOf(HTMLVideoElement);
    // Saving goes through the shell. A cross-origin `<a download>` would navigate the window to the
    // file instead of saving it, leaving the app replaced by a blank media document.
    expect(within(details!).queryByRole("link")).not.toBeInTheDocument();
    await userEvent.click(within(details!).getByRole("button", { name: "Save Calm variation" }));
    expect(saveArtifact).toHaveBeenCalledWith("/exports/variation.mp4", "variation.mp4");
    await userEvent.click(within(details!).getByRole("button", { name: "Save Publish package" }));
    expect(saveArtifact).toHaveBeenCalledWith("/exports/publish.zip", "publish.zip");
    await userEvent.click(screen.getByRole("button", { name: "Save Portrait master" }));
    expect(saveArtifact).toHaveBeenCalledWith("/exports/master.mp4", "master.mp4");
    await userEvent.click(screen.getByRole("button", { name: "Open in Video Studio" }));
    expect(onOpen).toHaveBeenCalledWith("project-1");
  });
});
