export type Theme = "dark" | "light";

export type NavKey =
  | "generate"
  | "voices"
  | "models"
  | "live"
  | "compare"
  | "benchmarks"
  | "history"
  | "settings"
  | "about";

export interface CatalogModel {
  model_id: string;
  task: "tts" | "stt";
  engine: string;
  tier: "smoke" | "recommended" | "advanced";
  recommended_for_12gb: boolean;
  languages: string[];
  summary: string;
  known_limitations?: string[];
  source_urls?: string[];
  default_sample_rate?: number | null;
  access?: string;
}

export interface InstalledModel {
  model_id: string;
  task: "tts" | "stt";
  engine: string;
  tier: string;
  local_path: string;
  downloaded_at: string;
  languages: string[];
}

export interface SystemStatus {
  gpu_name: string;
  vram_total_mb: number;
  vram_used_mb: number;
  driver_version: string;
  cuda_available: boolean;
  python_ready: boolean;
}

export interface VoiceProfile {
  id: string;
  name: string;
  style: string;
  sample_label: string;
  sample_seconds: number;
  engines: string[];
  consent: "confirmed" | "pending" | "not-required";
  state: "ready" | "draft" | "preset";
  color: "green" | "amber" | "coral";
  local_path?: string;
}

export interface BootstrapState {
  catalog: CatalogModel[];
  installed: InstalledModel[];
  system: SystemStatus;
  export_dir: string;
  voices: VoiceProfile[];
  runtime: "tauri" | "browser";
}

export interface SynthesisRequest {
  model_id: string;
  text: string;
  speaker: string;
  language: string;
  reference_audio_path?: string;
  speed: number;
  seed: number;
  output_format: "wav" | "flac";
}

export interface SynthesisResult {
  id: string;
  model_id: string;
  engine: string;
  audio_path: string | null;
  sample_rate: number;
  duration_seconds: number;
  inference_seconds: number;
  rtf: number;
  vram_peak_mb: number;
  waveform: number[];
  created_at: string;
  preview: boolean;
}

export interface HistoryItem extends SynthesisResult {
  title: string;
  voice: string;
  text: string;
}

export interface BenchmarkResult {
  model: string;
  variant: string;
  rtf: number;
  ttfa: number;
  vramGb: number;
  quality: number;
}
