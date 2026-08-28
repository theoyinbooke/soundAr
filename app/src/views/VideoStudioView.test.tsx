import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";
import { VideoStudioView } from "./VideoStudioView";

afterEach(cleanup);

describe("VideoStudioView", () => {
  it("renders a playable and downloadable final master after preview and export", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft.*Just now/i }));
    await user.click(await screen.findByRole("button", { name: "Render preview" }));
    await user.click(await screen.findByRole("button", { name: "Export video" }));

    expect(await screen.findByRole("heading", { name: "Export complete" })).toBeVisible();
    expect(screen.getByLabelText("Final video: Creator update · Portrait master")).toHaveAttribute("src", expect.stringMatching(/^data:video\/mp4;base64,/));
    expect(screen.getByRole("link", { name: "Download master" })).toHaveAttribute("download", "creator-update-portrait-master.mp4");
    await user.click(screen.getByRole("button", { name: "Publish package" }));
    expect(await screen.findByRole("link", { name: "Download package" })).toHaveAttribute("download", "creator-update-publish-package.zip");
  });

  it("persists inspector changes through the shared revision service", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    render(<VideoStudioView service={service} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft.*Just now/i }));
    await user.click(await screen.findByRole("tab", { name: "Captions" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Caption style" }), "calm");
    const save = screen.getByRole("button", { name: "Save scene changes" });
    expect(save).toBeEnabled();
    await user.click(save);
    await screen.findByText(/Saved Hook: where I’ve been/i);

    const project = await service.getVideoProject("creator-update");
    expect(project.manifest.scenes[0].caption_style).toBe("calm");
    expect(project.manifest.revisions).toHaveLength(1);
  });
});
