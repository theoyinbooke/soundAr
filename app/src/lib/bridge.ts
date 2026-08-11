import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { fallbackBootstrap } from "../data";
import type { BootstrapState, SynthesisRequest, SynthesisResult } from "../types";

const hasTauriRuntime = () => "__TAURI_INTERNALS__" in window;

export async function loadBootstrapState(): Promise<BootstrapState> {
  if (!hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 260));
    return fallbackBootstrap;
  }

  return invoke<BootstrapState>("bootstrap_state");
}

export async function synthesizeSpeech(request: SynthesisRequest): Promise<SynthesisResult> {
  if (!hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 1200));
    const duration = Math.max(1.8, request.text.length / 13);
    return {
      id: crypto.randomUUID(),
      model_id: request.model_id,
      engine: request.model_id.includes("Kokoro") ? "kokoro" : "local-preview",
      audio_path: null,
      sample_rate: 24000,
      duration_seconds: duration,
      inference_seconds: duration * 0.21,
      rtf: 0.21,
      vram_peak_mb: 1126,
      waveform: Array.from({ length: 96 }, (_, index) => 0.18 + Math.abs(Math.sin(index * 0.47)) * 0.72),
      created_at: new Date().toISOString(),
      preview: true,
    };
  }

  return invoke<SynthesisResult>("synthesize", { request });
}

export async function loadGeneratedAudio(path: string, format: "wav" | "flac"): Promise<string> {
  if (!hasTauriRuntime()) return path;
  const payload = await invoke<ArrayBuffer | Uint8Array | number[]>("read_generated_audio", { path });
  const bytes = payload instanceof ArrayBuffer
    ? payload
    : payload instanceof Uint8Array
      ? payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength)
      : new Uint8Array(payload).buffer;
  return URL.createObjectURL(new Blob([bytes], { type: format === "flac" ? "audio/flac" : "audio/wav" }));
}

export function isDesktopRuntime(): boolean {
  return hasTauriRuntime();
}

export async function pickAudioFile(): Promise<string | undefined> {
  if (!hasTauriRuntime()) return undefined;
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Audio", extensions: ["wav", "flac", "mp3", "ogg", "m4a"] }],
  });
  return typeof selected === "string" ? selected : undefined;
}
