import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import type { HistoryItem, VoiceProfile } from "../types";
import { VoicesView } from "./VoicesView";

const bridge = vi.hoisted(() => ({
  addVoiceReference: vi.fn(),
  deleteVoiceProfile: vi.fn(),
  getAudioRecordingStatus: vi.fn(),
  importVoiceProfile: vi.fn(),
  listHistory: vi.fn(),
  loadGeneratedAudio: vi.fn(),
  loadVoiceAudio: vi.fn(),
  pickAudioFile: vi.fn(),
  processVoiceReference: vi.fn(),
  saveVoiceEvaluation: vi.fn(),
  startAudioRecording: vi.fn(),
  stopAudioRecording: vi.fn(),
  synthesizeSpeech: vi.fn(),
  transcribeAudio: vi.fn(),
  updateVoiceReferenceTranscript: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);

const reference = {
  id: "reference-1",
  original_path: "/managed/voice/original.wav",
  processed_path: "/managed/voice/processed.wav",
  original_sha256: "original-sha",
  processed_sha256: "processed-sha",
  analysis: {
    duration_seconds: 8,
    sample_rate: 24_000,
    channels: 1,
    peak_dbfs: -1,
    silence_ratio: 0.04,
    clipping_ratio: 0,
    waveform: Array.from({ length: 48 }, (_, index) => 0.2 + (index % 7) / 10),
  },
  processing: {
    schema_version: 2,
    selection_start_seconds: 0.2,
    selection_end_seconds: 7.8,
    remove_silence: true,
    normalize: true,
  },
  active: true,
  created_at: "2026-08-12T12:00:00Z",
  transcript_text: "The original phrase.",
  transcript_source: "automatic" as const,
  revision_count: 2,
};

const evaluation = {
  id: "evaluation-1",
  voice_id: "voice-1",
  reference_id: reference.id,
  model_id: "ResembleAI/chatterbox",
  history_id: "history-1",
  script: "Names, numbers, and emotion matter.",
  decision: "pending" as const,
  notes: "",
  created_at: "2026-08-12T12:00:00Z",
  updated_at: "2026-08-12T12:00:00Z",
};

const voice: VoiceProfile = {
  id: "voice-1",
  name: "Test Voice",
  style: "Documentary",
  sample_label: "Managed reference",
  sample_seconds: 8,
  engines: ["Chatterbox"],
  consent: "confirmed",
  state: "ready",
  color: "green",
  local_path: reference.processed_path,
  source_kind: "imported",
  references: [reference],
  evaluations: [evaluation],
};

const history: HistoryItem = {
  id: evaluation.history_id,
  model_id: evaluation.model_id,
  engine: "chatterbox",
  audio_path: "/managed/exports/evaluation.wav",
  sample_rate: 24_000,
  duration_seconds: 3,
  inference_seconds: 0.4,
  rtf: 0.13,
  vram_peak_mb: 4_000,
  waveform: [0.2, 0.7, 0.4],
  created_at: evaluation.created_at,
  preview: false,
  title: "Test Voice evaluation",
  voice: voice.name,
  text: evaluation.script,
};

beforeAll(() => {
  Object.defineProperty(URL, "revokeObjectURL", { configurable: true, value: vi.fn() });
  Object.defineProperty(HTMLMediaElement.prototype, "play", { configurable: true, value: vi.fn().mockResolvedValue(undefined) });
  Object.defineProperty(HTMLMediaElement.prototype, "pause", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("Voice Lab", () => {
  it("offers recording and upload sources with consent selected by default", async () => {
    const user = userEvent.setup();
    bridge.listHistory.mockResolvedValue([]);
    bridge.startAudioRecording.mockResolvedValue({ recording: true, device_name: "Studio mic" });
    bridge.getAudioRecordingStatus.mockResolvedValue({ recording: true, device_name: "Studio mic", duration_seconds: 0.4 });
    bridge.stopAudioRecording.mockResolvedValue({ recording: false, audio_path: "/captures/reference.wav", duration_seconds: 2.8 });
    bridge.pickAudioFile.mockResolvedValue("/uploads/reference.flac");

    render(
      <VoicesView
        bootstrap={{ ...fallbackBootstrap, voices: [voice], runtime: "tauri" }}
        voices={[voice]}
        onChange={vi.fn()}
        onGenerated={vi.fn()}
        onUseVoice={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Add voice profile" }));
    expect(screen.getByRole("checkbox", { name: /I confirm I own this voice/ })).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Record sample" }));
    expect(bridge.startAudioRecording).toHaveBeenCalledWith({ vad_enabled: true, auto_stop: false, silence_ms: 1200, input_gain: 1 });
    await user.click(screen.getByRole("button", { name: "Stop recording" }));
    expect(bridge.stopAudioRecording).toHaveBeenCalled();
    expect(screen.getByText("reference.wav")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Upload audio" }));
    expect(bridge.pickAudioFile).toHaveBeenCalled();
    expect(screen.getByText("reference.flac")).toBeVisible();
  });

  it("keeps preview visible and places secondary table actions in the overflow menu", async () => {
    const user = userEvent.setup();
    const onUseVoice = vi.fn();
    bridge.listHistory.mockResolvedValue([]);
    bridge.loadVoiceAudio.mockResolvedValue("blob:voice-preview");

    render(
      <VoicesView
        bootstrap={{ ...fallbackBootstrap, voices: [voice], runtime: "tauri" }}
        voices={[voice]}
        onChange={vi.fn()}
        onGenerated={vi.fn()}
        onUseVoice={onUseVoice}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Play Test Voice" }));
    expect(bridge.loadVoiceAudio).toHaveBeenCalledWith(reference.processed_path);

    await user.click(screen.getByRole("button", { name: "More actions for Test Voice" }));
    expect(screen.getByRole("menuitem", { name: "View details" })).toBeVisible();
    expect(screen.getByRole("menuitem", { name: "Delete profile" })).toBeVisible();
    await user.click(screen.getByRole("menuitem", { name: "Use voice" }));
    expect(onUseVoice).toHaveBeenCalledWith("voice-1");
  });

  it("edits references, persists transcripts, and reviews replayable model evidence", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    bridge.listHistory.mockResolvedValue([history]);
    bridge.loadGeneratedAudio.mockResolvedValue("blob:evaluation");
    bridge.processVoiceReference.mockResolvedValue(voice);
    bridge.updateVoiceReferenceTranscript.mockResolvedValue({
      ...voice,
      references: [{ ...reference, transcript_text: "Corrected phrase.", transcript_source: "corrected" }],
    });
    bridge.saveVoiceEvaluation.mockImplementation(async (value) => ({
      ...value,
      created_at: evaluation.created_at,
      updated_at: "2026-08-12T12:10:00Z",
    }));

    render(
      <VoicesView
        bootstrap={{ ...fallbackBootstrap, voices: [voice], runtime: "tauri" }}
        voices={[voice]}
        onChange={onChange}
        onGenerated={vi.fn()}
        onUseVoice={vi.fn()}
      />,
    );

    expect(screen.getByText(/2 revisions/)).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Edit" }));
    expect(screen.getByRole("button", { name: "Apply as new revision" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Apply as new revision" }));
    expect(bridge.processVoiceReference).toHaveBeenCalledWith("voice-1", "reference-1", expect.objectContaining({
      trim_start_seconds: 0.2,
      trim_end_seconds: 7.8,
      remove_silence: true,
      normalize: true,
    }));

    const transcript = screen.getByRole("textbox", { name: "Reference transcript" });
    await user.clear(transcript);
    await user.type(transcript, "Corrected phrase.");
    await user.click(screen.getByRole("button", { name: "Save correction" }));
    expect(bridge.updateVoiceReferenceTranscript).toHaveBeenCalledWith("voice-1", "reference-1", "Corrected phrase.", "corrected");

    await waitFor(() => expect(bridge.loadGeneratedAudio).toHaveBeenCalledWith(history.audio_path));
    const notes = screen.getByRole("textbox", { name: "Review notes" });
    await user.type(notes, "Clear likeness and pacing.");
    await user.click(screen.getByRole("button", { name: "Accept" }));
    expect(bridge.saveVoiceEvaluation).toHaveBeenCalledWith(expect.objectContaining({
      id: "evaluation-1",
      decision: "accepted",
      notes: "Clear likeness and pacing.",
    }));
    expect(onChange).toHaveBeenCalled();
  });
});
