import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import { GenerateView } from "./GenerateView";

const bridge = vi.hoisted(() => ({
  cancelBatchRun: vi.fn(),
  cancelJob: vi.fn(),
  clearFinishedJobs: vi.fn(),
  createBatchRun: vi.fn(),
  getSchedulerStatus: vi.fn(),
  importBatchInput: vi.fn(),
  listBatchRuns: vi.fn(),
  listHistory: vi.fn(),
  listJobs: vi.fn(),
  loadGeneratedAudio: vi.fn(),
  pauseBatchRun: vi.fn(),
  pickBatchInputFile: vi.fn(),
  queueBatchRun: vi.fn(),
  queueSynthesis: vi.fn(),
  resumeBatchRun: vi.fn(),
  retryJob: vi.fn(),
  saveGenerationPreset: vi.fn(),
  synthesizeSpeech: vi.fn(),
  updateBatchItem: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Generation batch import", () => {
  it("previews imported rows and preserves structured overrides when queued", async () => {
    const user = userEvent.setup();
    const bootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    const imported = {
      name: "localized-campaign",
      source_format: "csv" as const,
      rows: [
        { text: "Hello, listener.", name: "Intro", output_name: "0001-opening", priority: "urgent" as const, settings: { language: "en", speed: 0.9 } },
        { text: "Bonjour.", name: "French", output_name: "0002-french", settings: { language: "fr", seed: 9 } },
      ],
    };
    bridge.pickBatchInputFile.mockResolvedValue("/imports/localized.csv");
    bridge.importBatchInput.mockResolvedValue(imported);
    bridge.queueBatchRun.mockResolvedValue({
      id: "batch-1", name: imported.name, status: "queued", total_items: 2,
      completed_items: 0, failed_items: 0, request: { rows: imported.rows }, items: [],
      created_at: "2026-08-13T00:00:00Z", updated_at: "2026-08-13T00:00:00Z",
    });
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(bootstrap.scheduler);

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Batch" }));
    await user.click(screen.getByRole("button", { name: "Import TXT, CSV, or JSONL batch" }));

    expect(await screen.findByText("localized-campaign")).toBeVisible();
    expect(screen.getByText("2 rows / CSV")).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Script" })).toHaveValue("Hello, listener.\nBonjour.");
    await user.click(screen.getByRole("combobox", { name: "Queue priority" }));
    await user.click(screen.getByRole("option", { name: "Urgent" }));
    await user.click(screen.getByRole("button", { name: "Start batch" }));

    await waitFor(() => expect(bridge.queueBatchRun).toHaveBeenCalled());
    expect(bridge.queueBatchRun.mock.calls[0][0]).toBe("localized-campaign");
    expect(bridge.queueBatchRun.mock.calls[0][1]).toEqual(imported.rows);
    expect(bridge.queueBatchRun.mock.calls[0][3]).toBe(2);
    expect(bridge.queueBatchRun.mock.calls[0][4]).toBe("urgent");
  });
});
