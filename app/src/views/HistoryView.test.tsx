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
  it("keeps videos out of History: they open in Video Studio, not here", async () => {
    // A video, draft or finished, is Video Studio's; History holds audio generations. A master
    // grid here used to sit above every record and pulled a click on a video into this page.
    render(<VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={vi.fn()}>
      <HistoryView history={[]} onChange={vi.fn()} />
    </VideoIntegrationProvider>);

    expect(screen.queryByRole("heading", { name: "Video masters" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/^Play Creator update/)).not.toBeInTheDocument();
    expect(screen.getByText(/Videos live in Video Studio and Projects/)).toBeInTheDocument();
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
