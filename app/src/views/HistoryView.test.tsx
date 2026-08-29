import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { HistoryItem } from "../types";
import { HistoryView } from "./SecondaryViews";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";
import { VideoIntegrationProvider } from "../components/video/VideoIntegrationContext";

const bridge = vi.hoisted(() => ({
  listHistory: vi.fn(),
}));

vi.mock("../lib/bridge", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/bridge")>(),
  listHistory: bridge.listHistory,
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const damaged: HistoryItem = {
  id: "damaged-history",
  job_id: "job-1",
  model_id: "hexgrad/Kokoro-82M",
  engine: "kokoro",
  audio_path: "/managed/exports/damaged.wav",
  sample_rate: 24_000,
  duration_seconds: 2.4,
  inference_seconds: 0.2,
  rtf: 0.08,
  vram_peak_mb: 512,
  waveform: [0.2, 0.8],
  created_at: "2026-08-12T12:00:00Z",
  preview: false,
  title: "Damaged artifact",
  voice: "Heart",
  text: "This file changed after generation.",
  artifact_state: "modified",
};

describe("History artifact integrity", () => {
  it("keeps final video masters playable, downloadable, and linked to Video Studio", async () => {
    const user = userEvent.setup();
    const onOpenProject = vi.fn();
    render(<VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={onOpenProject}>
      <HistoryView history={[]} onChange={vi.fn()} />
    </VideoIntegrationProvider>);

    const player = await screen.findByLabelText("Play Creator update · Portrait master");
    const card = player.closest("article");
    expect(card).not.toBeNull();
    // Exports are saved through the shell, never through an anchor: a cross-origin `<a download>`
    // navigates the window to the file rather than saving it.
    expect(within(card!).queryByRole("link")).not.toBeInTheDocument();
    // Browser preview has no filesystem, so there is nothing to save — and never an `<a download>`,
    // which the desktop webview would follow as a navigation instead of a save.
    expect(within(card!).queryByRole("button", { name: /^Save / })).not.toBeInTheDocument();
    await user.click(within(card!).getByRole("button", { name: "Open in Video Studio" }));
    expect(onOpenProject).toHaveBeenCalledWith("creator-update-master");
  });

  it("plays the selected master in place instead of sending the user to the editor", async () => {
    // Choosing finished work from the sidebar lands here, on the master's own player. Opening the
    // Video Studio editor stays an explicit, separate action on the card.
    const onOpenProject = vi.fn();
    Element.prototype.scrollIntoView = vi.fn();
    const service = createBrowserPreviewVideoService();
    const projects = await service.listVideoProjects();
    const target = projects.find((project) => project.master);
    expect(target).toBeDefined();

    render(<VideoIntegrationProvider service={service} onOpenProject={onOpenProject} activeProjectId={target!.id}>
      <HistoryView history={[]} onChange={vi.fn()} />
    </VideoIntegrationProvider>);

    const player = await screen.findByLabelText(`Play ${target!.master!.title}`);
    expect(player).toBeInstanceOf(HTMLVideoElement);
    expect(player).toHaveAttribute("controls");
    const card = player.closest("article");
    expect(card).toHaveClass("is-selected");
    expect(card).toHaveAttribute("aria-current", "true");
    expect(card!.scrollIntoView).toHaveBeenCalled();
    // Reaching the player must not have opened the editor.
    expect(onOpenProject).not.toHaveBeenCalled();
  });

  it("explains a modified artifact and disables playback and reveal", () => {
    bridge.listHistory.mockResolvedValue([damaged]);
    render(<HistoryView history={[damaged]} onChange={vi.fn()} />);

    expect(screen.getByText("Audio file changed on disk")).toBeVisible();
    expect(screen.getByRole("button", { name: "Audio file changed on disk" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "More actions for Damaged artifact" }));
    expect(screen.getByRole("menuitem", { name: "Duplicate artifact" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Export copy" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Reveal in folder" })).toBeDisabled();
  });
});
