import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VideoArtifact, VideoProjectSummary } from "../../types/video";
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
      duration_ms: 4_000,
      scene_count: 2,
      updated_at: "2026-08-27T20:00:00Z",
      master,
      deliverables: [master, variation, publish],
    };
    const onOpen = vi.fn();
    render(<VideoMasterCard project={project} variant="history" onOpen={onOpen} />);

    expect(screen.getByLabelText("Play Portrait master")).toBeInstanceOf(HTMLVideoElement);
    const details = screen.getByText("2 additional deliverables").closest("details");
    expect(details).not.toHaveAttribute("open");
    expect(screen.queryByLabelText("Play Calm variation")).not.toBeInTheDocument();
    await userEvent.click(within(details!).getByText("2 additional deliverables"));
    expect(within(details!).getByLabelText("Play Calm variation")).toBeInstanceOf(HTMLVideoElement);
    expect(within(details!).getByRole("link", { name: "Download Calm variation" })).toHaveAttribute("download", "variation.mp4");
    expect(within(details!).getByRole("link", { name: "Download Publish package" })).toHaveAttribute("download", "publish.zip");
    await userEvent.click(screen.getByRole("button", { name: "Open in Video Studio" }));
    expect(onOpen).toHaveBeenCalledWith("project-1");
  });
});
