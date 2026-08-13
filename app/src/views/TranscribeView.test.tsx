import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import type { TranscriptionRecord } from "../types";
import { TranscribeView } from "./TranscribeView";

const bridge = vi.hoisted(() => ({
  loadTranscriptionAudio: vi.fn(),
  pickAudioFile: vi.fn(),
  transcribeAudio: vi.fn(),
  updateTranscription: vi.fn(),
  alignTranscription: vi.fn(),
  diarizeTranscription: vi.fn(),
  updateTranscriptionSpeakerLabels: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

bridge.loadTranscriptionAudio.mockResolvedValue("data:audio/wav;base64,UklGRg==");

const receipt: TranscriptionRecord = {
  id: "transcript-1",
  job_id: "job-1",
  source_path: "/managed/cleaned.wav",
  original_source_path: "/managed/original.wav",
  model_id: "openai/whisper-tiny",
  engine: "transformers",
  text: "Measured local transcript",
  segments: [],
  words: [
    { text: "Measured", start_seconds: 0.2, end_seconds: 0.6 },
    { text: "local", start_seconds: 0.6, end_seconds: 1.0 },
    {
      text: "transcript",
      start_seconds: 1.0,
      end_seconds: 1.7,
      end_inferred: true,
    },
  ],
  detected_language: "en",
  language_confidence: 0.996,
  evidence: {
    schema_version: 1,
    timing_source: "whisper-token-alignment",
    language_source: "whisper-decoder-logits",
    word_confidence_source: "unavailable",
  },
  audio_duration_seconds: 2,
  inference_seconds: 0.2,
  rtf: 0.1,
  vram_peak_mb: 400,
  created_at: "2026-08-13T00:00:00Z",
  processing: {
    algorithm: "soundar-speech-cleanup-v1",
    noise_floor_before_dbfs: -38.4,
    noise_floor_after_dbfs: -49.2,
    gated_frame_ratio: 0.42,
  },
};

describe("Transcribe workflow", () => {
  it("shows persisted cleanup evidence for the exact derived source", () => {
    render(
      <TranscribeView
        bootstrap={fallbackBootstrap}
        records={[receipt]}
        onChange={vi.fn()}
      />,
    );
    expect(
      screen.getByText("Speech cleanup", {
        selector: ".processing-receipt span",
      }),
    ).toBeVisible();
    expect(screen.getByText("-38.4 to -49.2 dBFS")).toBeVisible();
    expect(screen.getByText(/42% frames attenuated/)).toBeVisible();
  });

  it("shows truthful language and word timing evidence without inventing confidence", () => {
    render(
      <TranscribeView
        bootstrap={fallbackBootstrap}
        records={[receipt]}
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByText("EN / 99.6%")).toBeVisible();
    expect(screen.getByText("3 aligned")).toBeVisible();
    expect(
      screen.getByText("Not reported", {
        selector: ".transcription-evidence strong",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /Play from 0:00.2/ }),
    ).toBeVisible();
  });

  it("sends cleanup only after explicit opt-in", async () => {
    bridge.pickAudioFile.mockResolvedValue("/source/interview.wav");
    bridge.transcribeAudio.mockResolvedValue(receipt);
    bridge.loadTranscriptionAudio.mockResolvedValue("blob:cleaned");
    const user = userEvent.setup();
    render(
      <TranscribeView
        bootstrap={fallbackBootstrap}
        records={[]}
        onChange={vi.fn()}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: /Choose audio to transcribe/ }),
    );
    await user.click(screen.getByRole("checkbox", { name: "Speech cleanup" }));
    await user.click(screen.getByRole("button", { name: "Transcribe audio" }));
    expect(bridge.transcribeAudio).toHaveBeenCalledWith(
      "openai/whisper-tiny",
      "/source/interview.wav",
      true,
    );
  });

  it("saves text corrections while preserving measured segment timing", async () => {
    const timedReceipt: TranscriptionRecord = {
      ...receipt,
      segments: [
        {
          text: "Measured local transcript",
          start_seconds: 0.2,
          end_seconds: 1.7,
        },
      ],
    };
    bridge.updateTranscription.mockResolvedValue({
      id: timedReceipt.id,
      text: "Corrected local transcript",
      segments: [
        {
          text: "Corrected local transcript",
          start_seconds: 0.2,
          end_seconds: 1.7,
        },
      ],
      revision_count: 1,
      updated_at: "2026-08-13T01:00:00Z",
    });
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(
      <TranscribeView
        bootstrap={fallbackBootstrap}
        records={[timedReceipt]}
        onChange={onChange}
      />,
    );

    const segment = screen.getByRole("textbox", {
      name: "Transcript segment 1",
    });
    await user.clear(segment);
    await user.type(segment, "Corrected local transcript");
    await user.click(screen.getByRole("button", { name: "Save correction" }));

    expect(bridge.updateTranscription).toHaveBeenCalledWith(
      timedReceipt.id,
      "Corrected local transcript",
      [
        {
          text: "Corrected local transcript",
          start_seconds: 0.2,
          end_seconds: 1.7,
        },
      ],
    );
    expect(onChange).toHaveBeenCalledWith([
      expect.objectContaining({
        text: "Corrected local transcript",
        revision_count: 1,
      }),
    ]);
    expect(screen.getByText("1 correction revision")).toBeVisible();
  });

  it("discloses provisional speaker evidence and persists edited labels", async () => {
    const diarized = fallbackBootstrap.transcriptions[0];
    bridge.updateTranscriptionSpeakerLabels.mockResolvedValue({
      labels: { "speaker-1": "Host", "speaker-2": "Speaker 2" },
      label_revision_count: 1,
      labels_updated_at: "2026-08-13T01:00:00Z",
    });
    const user = userEvent.setup();
    render(
      <TranscribeView
        bootstrap={fallbackBootstrap}
        records={[diarized]}
        onChange={vi.fn()}
      />,
    );

    expect(screen.getByText("Provisional clustering")).toBeVisible();
    expect(screen.getByText("Overlap not detected")).toBeVisible();
    expect(screen.getByText("No turn confidence")).toBeVisible();
    const speaker = screen.getByRole("textbox", { name: "Name Speaker 1" });
    await user.clear(speaker);
    await user.type(speaker, "Host");
    await user.click(screen.getByRole("button", { name: "Save speaker labels" }));
    expect(bridge.updateTranscriptionSpeakerLabels).toHaveBeenCalledWith(
      diarized.id,
      { "speaker-1": "Host", "speaker-2": "Speaker 2" },
    );
    expect(screen.getByText("Speaker labels revision 1 saved")).toBeVisible();
    expect(screen.getByRole("button", { name: "Play speaker turn 1" })).toBeVisible();
  });

  it("runs revision-linked alignment and discloses uncalibrated scores", async () => {
    const transcript = structuredClone(fallbackBootstrap.transcriptions[0]);
    transcript.alignment = null;
    const alignment = structuredClone(fallbackBootstrap.transcriptions[0].alignment!);
    bridge.alignTranscription.mockResolvedValue(alignment);
    const onChange = vi.fn();
    const user = userEvent.setup();
    render(
      <TranscribeView bootstrap={fallbackBootstrap} records={[transcript]} onChange={onChange} />,
    );

    await user.click(screen.getByRole("button", { name: "Align correction" }));
    expect(bridge.alignTranscription).toHaveBeenCalledWith(
      transcript.id,
      "facebook/wav2vec2-base-960h",
    );
    expect(screen.getByText("Scores uncalibrated")).toBeVisible();
    expect(screen.getByText("Aligned revision 0")).toBeVisible();
    expect(onChange).toHaveBeenCalledWith([expect.objectContaining({ alignment })]);
  });
});
