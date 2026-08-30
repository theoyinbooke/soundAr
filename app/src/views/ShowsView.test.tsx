import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ShowsView } from "./ShowsView";
import { VideoIntegrationProvider } from "../components/video/VideoIntegrationContext";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";

afterEach(cleanup);

function renderShows(onOpenProject = vi.fn()) {
  return render(
    <VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={onOpenProject}>
      <ShowsView />
    </VideoIntegrationProvider>,
  );
}

describe("ShowsView", () => {
  it("reports what an episode's release is still waiting on", async () => {
    const user = userEvent.setup();
    renderShows();

    const row = await screen.findByRole("button", { name: /^Inspect Creator update · Reel master$/ });
    await user.click(row);

    // A blocked member has to name its missing prerequisite: a member that is merely absent from
    // the list looks the same as one that is finished, which is the mistake this surface exists to
    // prevent.
    const release = await screen.findByRole("region", { name: "Release" });
    const blocked = await within(release).findByText(/No line has been narrated yet, so there is no audio episode/i);
    expect(blocked).toBeVisible();
  });

  it("replaces the show list with the episode rather than appending it below", async () => {
    const user = userEvent.setup();
    renderShows();

    await user.click(await screen.findByRole("button", { name: /^Inspect Creator update · Reel master$/ }));

    // The episode owns the screen: the tables that led here are gone, not scrolled past.
    await screen.findByRole("region", { name: "Release" });
    expect(screen.queryByRole("button", { name: /^Inspect Creator update · Reel master$/ })).toBeNull();
    expect(screen.queryByRole("heading", { name: "Shows" })).toBeNull();

    // And going back restores the list rather than stranding the user on the episode.
    await user.click(screen.getByRole("button", { name: /All shows/ }));
    expect(await screen.findByRole("button", { name: /^Inspect Creator update · Reel master$/ })).toBeVisible();
  });

  it("says an episode has no picture and therefore cannot be packaged", async () => {
    const user = userEvent.setup();
    renderShows();

    await user.click(await screen.findByRole("button", { name: /^Inspect Creator update · Reel master$/ }));
    const picture = await screen.findByRole("region", { name: "Picture" });

    // Without a picture the episode cannot render at all, so the surface says that rather than
    // leaving an empty panel that reads as "nothing to do here".
    expect(within(picture).getByText(/cannot be rendered or packaged as video/i)).toBeVisible();
    expect(within(picture).getByRole("button", { name: /Draw cover/ })).toBeEnabled();
  });

  it("hands a selected episode to the editor rather than opening a second one", async () => {
    const user = userEvent.setup();
    const onOpenProject = vi.fn();
    renderShows(onOpenProject);

    await user.click(await screen.findByRole("button", { name: /^Inspect Creator update · Reel master$/ }));
    await user.click(await screen.findByRole("button", { name: "Open in Video Studio" }));

    await waitFor(() => expect(onOpenProject).toHaveBeenCalledTimes(1));
    expect(onOpenProject.mock.calls[0][0]).toEqual(expect.any(String));
  });
});
