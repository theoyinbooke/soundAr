import type {
  BenchmarkResult,
  BootstrapState,
  CatalogModel,
  InstalledModel,
  VoiceProfile,
} from "./types";

export const fallbackCatalog: CatalogModel[] = [
  {
    model_id: "hexgrad/Kokoro-82M",
    task: "tts",
    engine: "kokoro",
    tier: "recommended",
    recommended_for_12gb: true,
    languages: ["en", "en-gb", "ja", "zh", "ko", "fr", "es", "hi"],
    summary: "Lightweight 82M speech model with low-latency synthesis and 54 voices.",
    default_sample_rate: 24000,
    source_urls: ["https://huggingface.co/hexgrad/Kokoro-82M"],
  },
  {
    model_id: "ResembleAI/chatterbox",
    task: "tts",
    engine: "chatterbox",
    tier: "recommended",
    recommended_for_12gb: true,
    languages: ["en"],
    summary: "Expressive local synthesis with zero-shot voice cloning.",
    default_sample_rate: 24000,
    source_urls: ["https://huggingface.co/ResembleAI/chatterbox"],
  },
  {
    model_id: "coqui/XTTS-v2",
    task: "tts",
    engine: "coqui",
    tier: "recommended",
    recommended_for_12gb: true,
    languages: ["en", "es", "fr", "de", "it", "pt", "ar", "zh-cn", "ja", "ko"],
    summary: "Multilingual voice cloning with 17-language synthesis.",
    default_sample_rate: 24000,
    source_urls: ["https://huggingface.co/coqui/XTTS-v2"],
  },
  {
    model_id: "microsoft/speecht5_tts",
    task: "tts",
    engine: "transformers",
    tier: "smoke",
    recommended_for_12gb: true,
    languages: ["en"],
    summary: "Compact baseline model for validating local synthesis.",
    default_sample_rate: 16000,
    source_urls: ["https://huggingface.co/microsoft/speecht5_tts"],
  },
  {
    model_id: "openai/whisper-tiny",
    task: "stt",
    engine: "transformers",
    tier: "smoke",
    recommended_for_12gb: true,
    languages: ["multilingual"],
    summary: "Fast local transcription smoke-test model.",
    default_sample_rate: 16000,
    source_urls: ["https://huggingface.co/openai/whisper-tiny"],
  },
  {
    model_id: "nvidia/parakeet-tdt-1.1b",
    task: "stt",
    engine: "nemo",
    tier: "recommended",
    recommended_for_12gb: true,
    languages: ["en"],
    summary: "FastConformer-TDT English transcription model.",
    default_sample_rate: 16000,
    source_urls: ["https://huggingface.co/nvidia/parakeet-tdt-1.1b"],
  },
];

const installedIds = new Set([
  "hexgrad/Kokoro-82M",
  "ResembleAI/chatterbox",
  "coqui/XTTS-v2",
  "microsoft/speecht5_tts",
  "openai/whisper-tiny",
  "nvidia/parakeet-tdt-1.1b",
]);

export const fallbackInstalled: InstalledModel[] = fallbackCatalog
  .filter((model) => installedIds.has(model.model_id))
  .map((model) => ({
    model_id: model.model_id,
    task: model.task,
    engine: model.engine,
    tier: model.tier,
    local_path: `~/.soundAr/models/${model.model_id.replace("/", "__")}`,
    downloaded_at: "2026-03-30T17:05:09Z",
    languages: model.languages,
  }));

export const seedVoices: VoiceProfile[] = [
  {
    id: "mara",
    name: "Mara",
    style: "Warm documentary",
    sample_label: "Owner-recorded sample",
    sample_seconds: 18,
    engines: ["Kokoro", "Chatterbox"],
    consent: "confirmed",
    state: "ready",
    color: "green",
  },
  {
    id: "amara",
    name: "Amara",
    style: "Clear narration",
    sample_label: "Verified local sample",
    sample_seconds: 31,
    engines: ["Kokoro", "XTTS"],
    consent: "confirmed",
    state: "ready",
    color: "green",
  },
  {
    id: "studio-neutral",
    name: "Studio Neutral",
    style: "Utility preset",
    sample_label: "Built-in voice",
    sample_seconds: 0,
    engines: ["Kokoro"],
    consent: "not-required",
    state: "preset",
    color: "amber",
  },
  {
    id: "oyin-test",
    name: "Oyin — test",
    style: "Conversational",
    sample_label: "Sample needs review",
    sample_seconds: 12,
    engines: ["Chatterbox"],
    consent: "pending",
    state: "draft",
    color: "coral",
  },
];

export const seedBenchmarks: BenchmarkResult[] = [
  { model: "Kokoro 82M", variant: "ONNX fp16", rtf: 0.21, ttfa: 0.34, vramGb: 1.1, quality: 0.84 },
  { model: "F5-TTS", variant: "Base fp16", rtf: 0.31, ttfa: 0.82, vramGb: 5.8, quality: 0.89 },
  { model: "Chatterbox", variant: "Turbo fp16", rtf: 0.44, ttfa: 1.08, vramGb: 7.4, quality: 0.91 },
  { model: "XTTS v2", variant: "Default fp16", rtf: 0.67, ttfa: 1.44, vramGb: 4.6, quality: 0.87 },
  { model: "Fish Speech 1.5", variant: "Default fp16", rtf: 0.73, ttfa: 1.62, vramGb: 9.2, quality: 0.92 },
];

export const fallbackBootstrap: BootstrapState = {
  catalog: fallbackCatalog,
  installed: fallbackInstalled,
  system: {
    gpu_name: "NVIDIA GeForce RTX 4080 Laptop GPU",
    vram_total_mb: 12282,
    vram_used_mb: 1265,
    driver_version: "595.84",
    cuda_available: true,
    python_ready: true,
  },
  export_dir: "~/.soundAr/exports",
  voices: seedVoices,
  runtime: "browser",
};
