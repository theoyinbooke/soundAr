import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import { GenerateView } from "./GenerateView";

const bridge = vi.hoisted(() => ({
  cancelBatchRun: vi.fn(),
  cancelJob: vi.fn(),
  clearFinishedJobs: vi.fn(),
  createBatchRun: vi.fn(),
  generateMusic: vi.fn(),
  getAudioRecordingStatus: vi.fn(),
  getHistoryRequest: vi.fn(),
  getSchedulerStatus: vi.fn(),
  importVoiceProfile: vi.fn(),
  importBatchInput: vi.fn(),
  listBatchRuns: vi.fn(),
  listHistory: vi.fn(),
  listJobs: vi.fn(),
  loadGeneratedAudio: vi.fn(),
  pauseBatchRun: vi.fn(),
  pickAudioFile: vi.fn(),
  pickBatchInputFile: vi.fn(),
  pickMusicAudioFile: vi.fn(),
  queueBatchRun: vi.fn(),
  queueMusicGeneration: vi.fn(),
  queueSynthesis: vi.fn(),
  resumeBatchRun: vi.fn(),
  retryJob: vi.fn(),
  saveGenerationPreset: vi.fn(),
  startAudioRecording: vi.fn(),
  stopAudioRecording: vi.fn(),
  synthesizeSpeech: vi.fn(),
  updateBatchItem: vi.fn(),
  updateHistoryMetadata: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Generation batch import", () => {
  it("clears failed speech jobs without deleting generated history", async () => {
    const user = userEvent.setup();
    const failed = {
      id: "failed-breeze-job",
      kind: "synthesis",
      status: "failed" as const,
      progress: 0.05,
      attempt: 1,
      error: "CUDA out of memory. Tried to allocate 1.00 GiB.",
      title: "Broken Breeze job",
      model_id: "BreezeBlue/Breeze-TTS-2",
      priority: "normal" as const,
      created_at: "2026-08-27T00:00:00Z",
      updated_at: "2026-08-27T00:00:01Z",
    };
    const bootstrap = { ...fallbackBootstrap, jobs: [failed] };
    bridge.listJobs.mockResolvedValue([failed]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(bootstrap.scheduler);
    bridge.clearFinishedJobs.mockResolvedValue(1);

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /Completed 1/ }));
    expect(screen.getByText("Broken Breeze job")).toBeVisible();
    expect(screen.getByText("Not enough free GPU memory")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "More options for Broken Breeze job" }));
    await user.click(screen.getByRole("menuitem", { name: "Clear finished task list" }));

    await waitFor(() => expect(bridge.clearFinishedJobs).toHaveBeenCalledTimes(1));
    expect(screen.queryByText("Broken Breeze job")).not.toBeInTheDocument();
  });

  it("shows the qualified Breeze and Fish Speech installs in the speech model picker", async () => {
    const user = userEvent.setup();
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(fallbackBootstrap.scheduler);

    render(<GenerateView bootstrap={fallbackBootstrap} voices={fallbackBootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("combobox", { name: "Model" }));

    expect(screen.getByRole("option", { name: "BreezeBlue/Breeze-TTS-2" })).toBeVisible();
    expect(screen.getByRole("option", { name: "fishaudio/fish-speech-1.5" })).toBeVisible();
  });

  it("queues separate music direction and lyric conditions without voice-cloning fields", async () => {
    const user = userEvent.setup();
    const bootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(bootstrap.scheduler);
    bridge.queueMusicGeneration.mockImplementation(async (request) => ({
      id: `music-job-${request.variation_index + 1}`, kind: "music-generation", status: "preparing", progress: 0.05,
      attempt: 1, priority: "normal", title: "Warm indie-pop", model_id: "ACE-Step/acestep-v15-xl-turbo-diffusers",
      created_at: "2026-08-13T00:00:00Z", updated_at: "2026-08-13T00:00:00Z",
    }));

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Music" }));
    const information = screen.getByLabelText("Music studio information");
    expect(information.closest(".page-actions")).not.toBeNull();
    expect(document.querySelector(".music-info-row")).toBeNull();
    expect(screen.getByRole("textbox", { name: "Direction" })).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Verse 1 lyrics" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Generate 2 variations" }));

    await waitFor(() => expect(bridge.queueMusicGeneration).toHaveBeenCalledTimes(2));
    expect(bridge.queueMusicGeneration).toHaveBeenNthCalledWith(1, expect.objectContaining({
      model_id: "ACE-Step/Ace-Step1.5",
      prompt: expect.stringContaining("Warm, intimate indie-pop"),
      lyrics: expect.stringContaining("[Verse 1]"),
      vocal_language: "en",
      duration_seconds: 90,
      inference_steps: 8,
      shift: 3,
      bpm: 0,
      output_format: "wav",
      planner_enabled: true,
      variations: 2,
      variation_index: 0,
    }));
    expect(bridge.queueMusicGeneration.mock.calls[0][0]).not.toHaveProperty("reference_audio_path");
    expect(bridge.queueMusicGeneration.mock.calls[0][0]).not.toHaveProperty("speaker");
    expect(bridge.queueMusicGeneration.mock.calls[0][0]).not.toHaveProperty("guidance_scale");
  });

  it("does not let MusicGen silently ignore lyric text", async () => {
    const user = userEvent.setup();
    const bootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(bootstrap.scheduler);

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Music" }));
    await user.click(screen.getByRole("combobox", { name: "Music model" }));
    await user.click(screen.getByRole("option", { name: "facebook/musicgen-small" }));

    expect(screen.getByText(/instrumental-only\. Choose ACE-Step or switch to Instrumental/i)).toBeVisible();
    expect(screen.getByRole("button", { name: "Generate 2 variations" })).toBeDisabled();
  });

  it("shows model cost, fit, license, speed, and capabilities before setup", async () => {
    const user = userEvent.setup();
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(fallbackBootstrap.scheduler);

    render(<GenerateView bootstrap={fallbackBootstrap} voices={fallbackBootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Music" }));
    await user.click(screen.getByRole("button", { name: "Studio setup" }));

    expect(screen.getByRole("dialog", { name: "Music studio setup" })).toBeVisible();
    expect(screen.getByText("~9 GB download")).toBeVisible();
    expect(screen.getByText("Near real time after warm-up")).toBeVisible();
    expect(screen.getByText("Songs · lyrics · references · extend · repaint")).toBeVisible();
    expect(screen.getAllByText(/MIT · public/i).length).toBeGreaterThan(0);
  });

  it("requires and forwards provenance for reference-guided source editing", async () => {
    const user = userEvent.setup();
    const bootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    bridge.listJobs.mockResolvedValue([]);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listBatchRuns.mockResolvedValue([]);
    bridge.getSchedulerStatus.mockResolvedValue(bootstrap.scheduler);
    bridge.pickMusicAudioFile.mockResolvedValueOnce("/audio/source.wav").mockResolvedValueOnce("/audio/reference.wav");
    bridge.queueMusicGeneration.mockImplementation(async (request) => ({
      id: `music-edit-${request.variation_index + 1}`, kind: "music-generation", status: "preparing", progress: 0.05,
      attempt: 1, priority: "normal", title: "Reference edit", model_id: "ACE-Step/Ace-Step1.5",
      created_at: "2026-08-27T00:00:00Z", updated_at: "2026-08-27T00:00:00Z",
    }));

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Music" }));
    await user.click(screen.getByRole("button", { name: "Extend" }));
    await user.click(screen.getByRole("button", { name: /Choose source audio/i }));
    await user.click(screen.getByRole("button", { name: /Add style reference/i }));

    expect(screen.getByRole("button", { name: "Generate 2 variations" })).toBeDisabled();
    await user.click(screen.getByRole("checkbox", { name: /I own or have permission/i }));
    await user.type(screen.getByRole("textbox", { name: "Reference audio permission basis" }), "My original studio recording");
    await user.click(screen.getByRole("button", { name: "Generate 2 variations" }));

    await waitFor(() => expect(bridge.queueMusicGeneration).toHaveBeenCalledTimes(2));
    expect(bridge.queueMusicGeneration).toHaveBeenNthCalledWith(1, expect.objectContaining({
      mode: "extend",
      source_audio_path: "/audio/source.wav",
      reference_audio_path: "/audio/reference.wav",
      reference_consent_confirmed: true,
      reference_consent_basis: "My original studio recording",
    }));
  });

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

    render(<GenerateView bootstrap={bootstrap} voices={bootstrap.voices} onVoicesChange={vi.fn()} onGenerated={vi.fn()} />);
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

  it("creates and selects a clone-ready voice directly from Generate", async () => {
    const user = userEvent.setup();
    const created = {
      id: "studio-voice",
      name: "Studio Voice",
      style: "Natural narration",
      sample_label: "Managed reference",
      sample_seconds: 6.4,
      engines: ["Chatterbox", "XTTS"],
      consent: "confirmed" as const,
      state: "ready" as const,
      color: "green" as const,
      local_path: "/managed/voices/studio-voice/reference.wav",
      source_kind: "recorded" as const,
    };
    bridge.pickAudioFile.mockResolvedValue("/uploads/reference.wav");
    bridge.importVoiceProfile.mockResolvedValue(created);

    function Harness() {
      const [voices, setVoices] = useState(fallbackBootstrap.voices);
      return <GenerateView bootstrap={fallbackBootstrap} voices={voices} onVoicesChange={setVoices} onGenerated={vi.fn()} />;
    }

    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "Add voice profile from Generate" }));
    expect(screen.getByRole("checkbox", { name: /I confirm I own this voice/ })).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Upload audio" }));
    await user.type(screen.getByPlaceholderText("Voice name"), created.name);
    await user.type(screen.getByPlaceholderText("Warm documentary"), created.style);
    await user.type(screen.getByPlaceholderText("Recorded by me, or written permission details"), "Recorded by me");
    await user.click(screen.getByRole("button", { name: "Create profile" }));

    await waitFor(() => expect(bridge.importVoiceProfile).toHaveBeenCalled());
    expect(bridge.importVoiceProfile).toHaveBeenCalledWith(expect.objectContaining({
      name: created.name,
      source_path: "/uploads/reference.wav",
      consent_confirmed: true,
    }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Model" })).toHaveTextContent("ResembleAI/chatterbox"));
    expect(screen.getByRole("combobox", { name: "Voice" })).toHaveTextContent("Studio Voice - Natural narration");
    expect(screen.queryByRole("dialog", { name: "Add voice profile" })).not.toBeInTheDocument();
  });
});
