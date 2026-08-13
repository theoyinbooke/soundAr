import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import { LiveView, reconcileAudioDeviceSelection } from "./SecondaryViews";

const bridge = vi.hoisted(() => ({
  getAudioRecordingStatus: vi.fn(),
  getAudioPlaybackStatus: vi.fn(),
  listAudioInputDevices: vi.fn(),
  listAudioOutputDevices: vi.fn(),
  loadTranscriptionAudio: vi.fn(),
  startAudioRecording: vi.fn(),
  startAudioPlayback: vi.fn(),
  stopAudioPlayback: vi.fn(),
  stopAudioRecording: vi.fn(),
  transcribeAudio: vi.fn(),
}));

vi.mock("../lib/bridge", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/bridge")>(),
  ...bridge,
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function prepare() {
  bridge.listAudioInputDevices.mockResolvedValue([{ id: "studio-mic", name: "Studio mic", is_default: true, sample_rate: 48_000, channels: 2, sample_format: "f32" }]);
  bridge.listAudioOutputDevices.mockResolvedValue([{ id: "studio-output", name: "Studio output", is_default: true, sample_rate: 48_000, channels: 2, sample_format: "f32" }]);
  bridge.getAudioRecordingStatus.mockResolvedValue({ recording: false });
  bridge.getAudioPlaybackStatus.mockResolvedValue({ playing: false });
  bridge.startAudioRecording.mockResolvedValue({ recording: true, device_name: "Studio mic", vad_enabled: true, auto_stop: true, input_gain: 1.5 });
}

describe("Live capture", () => {
  it("preserves valid device choices and falls back after disconnect", () => {
    const devices = [
      { id: "fallback", is_default: true },
      { id: "preferred", is_default: false },
    ];
    expect(reconcileAudioDeviceSelection("preferred", devices)).toBe("preferred");
    expect(reconcileAudioDeviceSelection("disconnected", devices)).toBe("fallback");
    expect(reconcileAudioDeviceSelection("disconnected", [])).toBe("");
  });

  it("sends the selected VAD, silence, gain, and device controls to native capture", async () => {
    prepare();
    const user = userEvent.setup();
    render(<LiveView bootstrap={fallbackBootstrap} />);
    await screen.findByRole("button", { name: "Record" });

    await user.click(screen.getByRole("checkbox", { name: "Stop after silence" }));
    const gain = screen.getByRole("slider", { name: "Input gain" });
    fireEvent.change(gain, { target: { value: "1.5" } });
    await user.click(screen.getByRole("combobox", { name: "Trailing silence" }));
    await user.click(screen.getByRole("option", { name: "2.0 seconds" }));
    await user.click(screen.getByRole("button", { name: "Record" }));

    expect(bridge.startAudioRecording).toHaveBeenCalledWith({
      device_id: "studio-mic",
      vad_enabled: true,
      auto_stop: true,
      silence_ms: 2_000,
      input_gain: 1.5,
    });
  });

  it("locks dependent controls when voice detection is disabled", async () => {
    prepare();
    const user = userEvent.setup();
    render(<LiveView bootstrap={fallbackBootstrap} />);
    await screen.findByRole("button", { name: "Record" });

    await user.click(screen.getByRole("checkbox", { name: "Voice detection" }));
    expect(screen.getByRole("checkbox", { name: "Stop after silence" })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Trailing silence" })).toBeDisabled();
  });

  it("disables empty audio device selectors without hiding their state", async () => {
    prepare();
    bridge.listAudioInputDevices.mockResolvedValue([]);
    bridge.listAudioOutputDevices.mockResolvedValue([]);
    render(<LiveView bootstrap={fallbackBootstrap} />);
    expect(await screen.findByRole("combobox", { name: "Audio input" })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: "Audio output" })).toBeDisabled();
    expect(screen.getByText("No input")).toBeVisible();
    expect(screen.getByText("No output")).toBeVisible();
  });

  it("loads an auto-stopped capture and keeps it available for transcription", async () => {
    prepare();
    bridge.startAudioRecording.mockResolvedValue({ recording: true, device_name: "Studio mic" });
    bridge.getAudioRecordingStatus
      .mockResolvedValueOnce({ recording: false })
      .mockResolvedValueOnce({ recording: false, audio_path: "/captures/take.wav", duration_seconds: 2.4, stop_reason: "silence", speech_seconds: 1.1 });
    bridge.loadTranscriptionAudio.mockResolvedValue("blob:capture");
    const user = userEvent.setup();
    render(<LiveView bootstrap={fallbackBootstrap} />);
    await screen.findByRole("button", { name: "Record" });
    await user.click(screen.getByRole("button", { name: "Record" }));

    await waitFor(() => expect(bridge.loadTranscriptionAudio).toHaveBeenCalledWith("/captures/take.wav"), { timeout: 1_000 });
    expect(await screen.findByText("Auto-stopped after trailing silence")).toBeVisible();
    expect(screen.getByRole("button", { name: "Transcribe capture" })).toBeEnabled();
  });

  it("routes a completed capture to the selected native output", async () => {
    prepare();
    bridge.getAudioRecordingStatus.mockResolvedValue({ recording: false, audio_path: "/captures/take.wav", duration_seconds: 2.4 });
    bridge.loadTranscriptionAudio.mockResolvedValue("blob:capture");
    bridge.startAudioPlayback.mockResolvedValue({ playing: true, device_name: "Studio output", progress: 0, underrun_frames: 0 });
    const user = userEvent.setup();
    render(<LiveView bootstrap={fallbackBootstrap} />);

    await user.click(await screen.findByRole("button", { name: "Play on output" }));
    expect(bridge.startAudioPlayback).toHaveBeenCalledWith("/captures/take.wav", "studio-output");
    expect(await screen.findByRole("button", { name: "Stop output" })).toBeVisible();
  });

  it("reports a routed output disconnect instead of silently stopping", async () => {
    prepare();
    bridge.getAudioPlaybackStatus
      .mockResolvedValueOnce({ playing: true, device_name: "Studio output", progress: 0.4 })
      .mockResolvedValue({ playing: false, playback_error: "Audio output stopped: device unavailable" });
    render(<LiveView bootstrap={fallbackBootstrap} />);

    expect(await screen.findByText("Audio output stopped: device unavailable")).toBeVisible();
    expect(screen.getByRole("button", { name: "Record" })).toBeEnabled();
  });

  it("passes speech cleanup only when the user opts in", async () => {
    prepare();
    bridge.getAudioRecordingStatus.mockResolvedValue({ recording: false, audio_path: "/captures/take.wav", duration_seconds: 2.4 });
    bridge.loadTranscriptionAudio.mockResolvedValue("blob:capture");
    bridge.transcribeAudio.mockResolvedValue({ text: "Clean transcript" });
    const user = userEvent.setup();
    render(<LiveView bootstrap={fallbackBootstrap} />);

    await user.click(await screen.findByRole("checkbox", { name: "Speech cleanup" }));
    await user.click(screen.getByRole("button", { name: "Transcribe capture" }));
    expect(bridge.transcribeAudio).toHaveBeenCalledWith("openai/whisper-tiny", "/captures/take.wav", true);
    expect(await screen.findByText("Clean transcript")).toBeVisible();
  });
});
