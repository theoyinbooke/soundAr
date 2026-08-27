export type Theme = "dark" | "light";
export type FeatureState = "stable" | "beta" | "experimental" | "disabled";
export type QueuePriority = "low" | "normal" | "high" | "urgent";
export type GenerationKind = "speech" | "music";

export type NavKey =
  | "generate"
  | "projects"
  | "voices"
  | "models"
  | "compare"
  | "benchmarks"
  | "history"
  | "settings"
  | "about";

export interface CatalogModel {
  model_id: string;
  task: "tts" | "stt" | "speaker-verification" | "alignment" | "music";
  engine: string;
  tier: "smoke" | "recommended" | "advanced";
  recommended_for_12gb: boolean;
  languages: string[];
  summary: string;
  known_limitations?: string[];
  source_urls?: string[];
  default_sample_rate?: number | null;
  license?: string;
  access?: string;
  install_status?: "ready" | "planned";
  revision?: string;
}

export interface InstalledModel {
  model_id: string;
  task: "tts" | "stt" | "speaker-verification" | "alignment" | "music";
  engine: string;
  tier: string;
  local_path: string;
  downloaded_at: string;
  languages: string[];
  revision?: string;
  download_size_bytes?: number;
  installed_size_bytes?: number;
  license?: string;
  integrity?: ModelIntegrity;
  file_manifest?: Array<{ filename: string; size: number }>;
}

export interface ModelIntegrity {
  model_id?: string;
  state: "ready" | "repair-needed" | "not-installed";
  reason: "verified" | "missing-directory" | "incomplete-files" | "not-installed";
  missing_files: string[];
  invalid_files: string[];
  checked_files: number;
  installed_size_bytes: number;
  manifest_verified: boolean;
}

export interface ModelInstallPlan {
  model_id: string;
  source_url: string;
  revision: string;
  license: string;
  access: "public" | "gated";
  download_size_bytes: number;
  file_count: number;
  recommended_for_12gb: boolean;
  model_cache_dir: string;
}

export interface ModelDownloadProgress {
  model_id: string;
  downloaded_bytes: number;
  total_bytes: number;
}

export interface SystemStatus {
  gpu_name: string;
  vram_total_mb: number;
  vram_used_mb: number;
  driver_version: string;
  cuda_available: boolean;
  python_ready: boolean;
}

export interface SchedulerStatus {
  active_workers: number;
  max_workers: number;
  reserved_vram_mb: number;
  available_vram_budget_mb?: number | null;
  active_batches: number;
  waiting_jobs?: number;
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
  source_kind?: "preset" | "imported" | "recorded";
  consent_basis?: string;
  speaker_relationship?: string;
  permitted_uses?: string;
  source_date?: string;
  analysis?: AudioAnalysis;
  references?: VoiceReference[];
  evaluations?: VoiceEvaluation[];
}

export interface VoiceReference {
  id: string;
  original_path: string;
  processed_path?: string | null;
  original_sha256: string;
  processed_sha256?: string | null;
  analysis: AudioAnalysis & { processing_error?: string };
  processing: {
    schema_version?: number;
    source_sample_rate?: number;
    output_sample_rate?: number;
    mono?: boolean;
    edge_trim_db?: number;
    trim_start_seconds?: number;
    trim_end_seconds?: number;
    peak_target_dbfs?: number;
    gain_db?: number;
    remove_silence?: boolean;
    normalize?: boolean;
    selection_start_seconds?: number;
    selection_end_seconds?: number;
  };
  active: boolean;
  created_at: string;
  transcript_text?: string;
  transcript_source?: "none" | "automatic" | "corrected";
  revision_count?: number;
}

export interface VoiceReferenceEdits {
  trim_start_seconds: number;
  trim_end_seconds: number;
  remove_silence: boolean;
  normalize: boolean;
  peak_target_dbfs: number;
}

export interface VoiceEvaluation {
  id: string;
  voice_id: string;
  reference_id: string;
  model_id: string;
  history_id: string;
  script: string;
  decision: "pending" | "accepted" | "rejected";
  notes: string;
  speaker_similarity?: number | null;
  similarity_model_id?: string | null;
  similarity_engine?: string | null;
  similarity_scoring_version?: string | null;
  similarity_inference_seconds?: number | null;
  similarity_vram_mb?: number | null;
  reference_sha256?: string | null;
  candidate_sha256?: string | null;
  similarity_measured_at?: string | null;
  created_at: string;
  updated_at: string;
}

export interface AudioAnalysis {
  format?: string;
  sample_rate?: number;
  channels?: number;
  duration_seconds?: number;
  peak_dbfs?: number;
  silence_ratio?: number;
  clipping_ratio?: number;
  waveform?: number[];
  warnings?: string[];
}

export interface VoiceImportRequest {
  name: string;
  style: string;
  source_path: string;
  consent_confirmed: boolean;
  consent_basis: string;
  speaker_relationship: string;
  permitted_uses: string;
  source_date: string;
}

export interface JobRecord {
  id: string;
  kind: string;
  status: "queued" | "preparing" | "running" | "completed" | "failed" | "cancelled";
  progress: number;
  stage?: "queued" | "preparing" | "planning" | "rendering" | "decoding" | "finalizing" | "completed";
  attempt: number;
  error?: string | null;
  title?: string;
  model_id?: string | null;
  priority?: QueuePriority;
  created_at: string;
  updated_at: string;
}

export interface EngineControl {
  type: "number";
  minimum: number;
  maximum: number;
  default: number;
}

export interface MusicFeatureCapability {
  lyrics: boolean;
  instrumental_when_lyrics_empty: boolean;
  planner?: boolean;
  reference_audio?: boolean;
  cover?: boolean;
  repaint?: boolean;
  extend?: boolean;
  lyric_timing?: boolean;
  stems?: boolean;
  max_variations?: number;
  max_duration_seconds?: number;
  max_lyrics_characters?: number;
  max_lyrics_characters_per_second?: number;
  sample_rate: number;
  channels: number;
}

export interface EngineCapability {
  id: string;
  display_name: string;
  adapter_version: number;
  tasks: string[];
  languages: string[];
  voice_modes: string[];
  streaming: boolean;
  reference_formats: string[];
  output_formats: string[];
  controls: Record<string, EngineControl>;
  music_features?: MusicFeatureCapability;
  transcription_evidence?: {
    word_timestamps: boolean;
    language_detection: boolean;
    declared_language: boolean;
    word_confidence: boolean;
  };
  diarization_evidence?: {
    word_anchored: boolean;
    editable_labels: boolean;
    overlap_detection: boolean;
    turn_confidence: boolean;
    provisional: boolean;
  };
  alignment_evidence?: {
    word_timestamps: boolean;
    acoustic_path_score: boolean;
    score_calibrated: boolean;
    source_revision_linked: boolean;
    provisional: boolean;
  };
  minimum_vram_mb: number;
  license: string;
}

export interface EngineRuntimeState {
  engine: string;
  state: "layered" | "legacy-shared" | "needs-setup";
  python_path: string;
  runtime_manifest: Record<string, string | number>;
  warm_workers: number;
  loaded_models: string[];
}

export interface EngineHealthState {
  status: string;
  device: string;
  engine_scope: string;
  engine_runtime: string;
  process_id: number;
  warm_workers: number;
  worker_starts: number;
  worker_restarts: number;
  worker_failures: number;
  last_started_at?: string | null;
  last_failure_at?: string | null;
  last_error?: string | null;
  loaded_models?: string[];
}

export interface ModelRuntimeAction {
  status: "loaded" | "unloaded";
  engine: string;
  model_id: string;
  task?: string;
  device?: string;
  retired_workers?: number;
  unloaded_models?: string[];
  vram?: { used_mb: number; total_mb: number; percent: number };
}

export interface DeveloperApiState {
  running: boolean;
  host?: string;
  port?: number;
  base_url?: string;
  token?: string;
}

export interface AudioInputDevice {
  id: string;
  name: string;
  is_default: boolean;
  sample_rate: number;
  channels: number;
  sample_format: string;
}

export type AudioOutputDevice = AudioInputDevice;

export interface AudioPlaybackState {
  playing: boolean;
  device_name?: string;
  audio_path?: string;
  duration_seconds?: number;
  played_seconds?: number;
  progress?: number;
  output_sample_rate?: number;
  startup_seconds?: number;
  elapsed_seconds?: number;
  underrun_frames?: number;
  playback_error?: string | null;
}

export interface AudioRecordingState {
  recording: boolean;
  device_name?: string;
  audio_path?: string;
  sample_rate?: number;
  channels?: number;
  duration_seconds?: number;
  peak?: number;
  speech_active?: boolean;
  speech_detected?: boolean;
  speech_seconds?: number;
  silence_seconds?: number;
  noise_floor?: number;
  dropped_frames?: number;
  buffered_frames?: number;
  vad_enabled?: boolean;
  auto_stop?: boolean;
  silence_ms?: number;
  input_gain?: number;
  stop_reason?: "user" | "silence" | "device" | null;
  capture_error?: string | null;
}

export interface AudioRecordingOptions {
  device_id?: string;
  vad_enabled: boolean;
  auto_stop: boolean;
  silence_ms: number;
  input_gain: number;
}

export interface GenerationPreset {
  id: string;
  name: string;
  schema_version: number;
  settings: Partial<SynthesisRequest>;
  created_at: string;
}

export interface BatchItemRecord {
  id: string;
  item_index: number;
  text: string;
  status: "queued" | "running" | "completed" | "failed" | "cancelled";
  history_id?: string | null;
  job_id?: string | null;
  name?: string;
  settings?: Partial<SynthesisRequest>;
  output_name?: string;
  attempt?: number;
  priority?: QueuePriority;
  error?: string | null;
  created_at: string;
  updated_at: string;
}

export interface BatchInputRow {
  text: string;
  name?: string;
  output_name?: string;
  settings?: Partial<SynthesisRequest>;
  priority?: QueuePriority;
}

export interface BatchImportResult {
  name: string;
  source_format: "txt" | "csv" | "jsonl";
  rows: Array<Required<Pick<BatchInputRow, "text">> & Pick<BatchInputRow, "name" | "output_name" | "settings">>;
}

export interface BatchRunRecord {
  id: string;
  name: string;
  status: "queued" | "running" | "paused" | "completed" | "failed" | "cancelled";
  paused?: boolean;
  total_items: number;
  completed_items: number;
  failed_items: number;
  priority?: QueuePriority;
  request: { scripts?: string[]; rows?: BatchInputRow[]; settings?: Partial<SynthesisRequest>; parallelism?: number; priority?: QueuePriority };
  error?: string | null;
  items: BatchItemRecord[];
  created_at: string;
  updated_at: string;
}

export interface ComparisonTakeRecord {
  id: string;
  position: number;
  label: string;
  request: SynthesisRequest;
  job_id?: string | null;
  history_id?: string | null;
  status: "queued" | "preparing" | "running" | "completed" | "failed" | "cancelled";
  rating?: number | null;
  notes: string;
  favorite: boolean;
  error?: string | null;
  result?: HistoryItem | null;
  created_at: string;
  updated_at: string;
}

export interface ComparisonRecord {
  id: string;
  script: string;
  status: "queued" | "running" | "completed" | "partial" | "failed" | "cancelled";
  blind: boolean;
  revealed: boolean;
  tie: boolean;
  winner_take_id?: string | null;
  promoted_take_id?: string | null;
  notes: string;
  takes: ComparisonTakeRecord[];
  created_at: string;
  updated_at: string;
}

export interface ProjectRecord {
  id: string;
  name: string;
  document: {
    script: string;
    chapters: ProjectChapter[];
    speaker_assignments: Record<string, unknown>;
    render_batch?: ProjectRenderBatch;
    master?: ProjectMasterArtifact;
    [key: string]: unknown;
  };
  created_at: string;
  updated_at: string;
}

export interface ProjectMasterArtifact {
  history_id?: string;
  audio_path: string;
  title?: string;
  duration_seconds?: number;
  sample_rate?: number;
  format?: "wav" | "flac";
  manifest_path?: string;
  created_at?: string;
}

export interface ProjectRenderBatch {
  batch_id: string;
  started_at: string;
  rows: Array<{
    chapter_id: string;
    item_index: number;
    source_text: string;
    source_model_id?: string | null;
    source_voice_id?: string | null;
    source_language?: string | null;
  }>;
}

export interface ProjectChapter {
  id: string;
  title: string;
  text: string;
  voice_id?: string;
  model_id?: string;
  language?: string;
  history_id?: string;
}

export interface ProjectMasterResult {
  history: HistoryItem;
  project: ProjectRecord;
  export: {
    id: string;
    project_id: string;
    history_id: string;
    settings: ProjectMasterSettings;
    manifest_path: string;
    created_at: string;
  };
}

export interface ProjectMasterSettings {
  format: "wav" | "flac";
  sample_rate: 24000 | 44100 | 48000;
  gap_ms: number;
  fade_ms: number;
  target_lufs: number;
}

export interface TranscriptionSegment {
  text: string;
  start_seconds: number;
  end_seconds: number;
}

export interface TranscriptionWord extends TranscriptionSegment {
  confidence?: number | null;
  end_inferred?: boolean;
}

export interface TranscriptionEvidence {
  schema_version: number;
  timing_source: "whisper-token-alignment" | "nemo-hypothesis" | "unavailable" | string;
  language_source: "whisper-decoder-logits" | "model-declared" | "unavailable" | string;
  word_confidence_source: "nemo-word-confidence" | "unavailable" | string;
  language_alternatives?: Array<{ language: string; probability: number }>;
}

export interface SpeakerDiarizationSpeaker {
  id: string;
  default_name: string;
}

export interface SpeakerDiarizationTurn {
  speaker_id: string;
  start_seconds: number;
  end_seconds: number;
  word_start_index: number;
  word_end_index: number;
  text: string;
  confidence?: number | null;
}

export interface SpeakerDiarizationEvidence {
  schema_version: number;
  method: string;
  clustering: string;
  distance_threshold: number;
  speaker_count_mode: "automatic" | "fixed";
  requested_speaker_count?: number | null;
  speech_window_count: number;
  overlap_detection: false;
  confidence_source: "unavailable";
  provisional: true;
}

export interface SpeakerDiarizationRecord {
  id: string;
  job_id: string;
  model_id: string;
  engine: string;
  speakers: SpeakerDiarizationSpeaker[];
  turns: SpeakerDiarizationTurn[];
  evidence: SpeakerDiarizationEvidence;
  inference_seconds: number;
  vram_peak_mb: number;
  labels: Record<string, string>;
  label_revision_count: number;
  labels_updated_at?: string | null;
  created_at: string;
}

export interface AlignmentWord extends TranscriptionSegment {
  alignment_score: number;
  segment_index: number;
}

export interface TranscriptionAlignmentRecord {
  id: string;
  job_id: string;
  model_id: string;
  engine: string;
  source_revision: number;
  source_text_sha256: string;
  words: AlignmentWord[];
  evidence: {
    schema_version: number;
    method: string;
    language: "en";
    source_revision_linked: true;
    score_source: string;
    score_calibrated: false;
    original_timestamps_preserved: true;
    provisional: true;
  };
  mean_alignment_score: number;
  inference_seconds: number;
  vram_peak_mb: number;
  current: boolean;
  created_at: string;
}

export interface TranscriptionRecord {
  id: string;
  job_id: string;
  source_path: string;
  original_source_path?: string;
  processing?: {
    schema_version?: number;
    algorithm?: string;
    sample_rate?: number;
    high_pass_hz?: number;
    gate_floor?: number;
    noise_floor_before_dbfs?: number;
    noise_floor_after_dbfs?: number;
    gated_frame_ratio?: number;
    normalization_gain_db?: number;
    original_peak_dbfs?: number;
  };
  model_id: string;
  engine: string;
  text: string;
  segments: TranscriptionSegment[];
  words: TranscriptionWord[];
  detected_language?: string | null;
  language_confidence?: number | null;
  evidence: TranscriptionEvidence;
  diarization?: SpeakerDiarizationRecord | null;
  alignment?: TranscriptionAlignmentRecord | null;
  original_text?: string;
  revision_count?: number;
  updated_at?: string;
  audio_duration_seconds: number;
  inference_seconds: number;
  rtf: number;
  vram_peak_mb: number;
  created_at: string;
}

export interface MeasuredBenchmarkRun {
  id: string;
  history_id?: string;
  transcription_id?: string;
  model_id: string;
  engine: string;
  inference_seconds: number;
  end_to_end_seconds?: number;
  runtime_overhead_seconds?: number;
  duration_seconds: number;
  rtf: number;
  vram_mb: number;
  created_at: string;
  source_text?: string;
  transcript?: string;
  word_error_rate?: number;
  character_error_rate?: number;
  word_errors?: number;
  reference_words?: number;
  character_errors?: number;
  reference_characters?: number;
  verifier_model_id?: string;
  verifier_engine?: string;
  source_sha256?: string;
  scoring_version?: string;
  warm_state?: "cold" | "warm";
  model_revision?: string;
  gpu_name?: string;
  driver_version?: string;
  app_version?: string;
}

export type RouteIntent = "manual" | "fast" | "expressive" | "clone" | "multilingual";

export interface BootstrapState {
  catalog: CatalogModel[];
  installed: InstalledModel[];
  system: SystemStatus;
  scheduler: SchedulerStatus;
  export_dir: string;
  voices: VoiceProfile[];
  jobs: JobRecord[];
  presets: GenerationPreset[];
  projects: ProjectRecord[];
  transcriptions: TranscriptionRecord[];
  benchmarks: MeasuredBenchmarkRun[];
  batches: BatchRunRecord[];
  comparisons: ComparisonRecord[];
  settings: ApplicationSettings;
  features: Record<string, FeatureState>;
  engine_capabilities: EngineCapability[];
  engine_runtimes: EngineRuntimeState[];
  install_kind: "appimage" | "deb" | "development";
  runtime: "tauri" | "browser";
}

export interface ApplicationSettings {
  theme: Theme;
  dense_tables: boolean;
  reduced_motion: boolean;
}

export interface UpdateCheckStatus {
  phase: "idle" | "checking" | "current" | "available" | "error" | "unavailable";
  message?: string;
}

export interface SynthesisRequest {
  model_id: string;
  text: string;
  input_mode?: "text" | "ssml";
  speaker: string;
  language: string;
  reference_audio_path?: string;
  speed: number;
  exaggeration?: number;
  cfg_weight?: number;
  temperature?: number;
  top_p?: number;
  repetition_penalty?: number;
  instruction?: string;
  cfg_scale?: number;
  seed: number;
  output_format: "wav" | "flac";
  title?: string;
  voice_name?: string;
  priority?: QueuePriority;
  benchmark_token?: string;
}

export interface MusicGenerationRequest {
  model_id: string;
  prompt: string;
  mode?: MusicStudioMode;
  quality_profile?: "balanced" | "highest";
  planner_enabled?: boolean;
  variations?: 1 | 2 | 4;
  variation_index?: number;
  lyrics?: string;
  song_sections?: MusicSongSection[];
  lyric_timing?: MusicLyricTiming[];
  vocal_language?: string;
  duration_seconds: number;
  guidance_scale?: number;
  temperature?: number;
  top_k?: number;
  top_p?: number;
  inference_steps?: number;
  shift?: number;
  bpm?: number;
  key_scale?: string;
  time_signature?: string;
  reference_audio_path?: string;
  source_audio_path?: string;
  reference_consent_confirmed?: boolean;
  reference_consent_basis?: string;
  repainting_start?: number;
  repainting_end?: number;
  audio_cover_strength?: number;
  stem_type?: "vocals" | "drums" | "bass" | "guitar" | "piano" | "other";
  return_lyric_timing?: boolean;
  return_stems?: boolean;
  parent_history_id?: string;
  seed: number;
  output_format: "wav" | "flac";
  title?: string;
  priority?: QueuePriority;
}

export type MusicStudioMode = "song" | "instrumental" | "extend" | "edit-region" | "cover" | "extract";

export interface MusicSongSection {
  id: string;
  type: "intro" | "verse" | "pre-chorus" | "chorus" | "bridge" | "instrumental" | "outro";
  label: string;
  lyrics: string;
  duration_seconds?: number;
}

export interface MusicLyricTiming {
  id: string;
  text: string;
  start_seconds: number;
  end_seconds: number;
}

export type GenerationRequest = SynthesisRequest | MusicGenerationRequest;

export interface SynthesisResult {
  id: string;
  model_id: string;
  engine: string;
  audio_path: string | null;
  sample_rate: number;
  duration_seconds: number;
  inference_seconds: number;
  runtime_worker_state?: "cold" | "warm" | "unknown";
  end_to_end_seconds?: number;
  runtime_overhead_seconds?: number;
  rtf: number;
  vram_peak_mb: number;
  waveform: number[];
  created_at: string;
  preview: boolean;
}

export interface HistoryItem extends SynthesisResult {
  job_id?: string;
  title: string;
  voice: string;
  text: string;
  generation_kind?: GenerationKind;
  missing?: boolean;
  artifact_state?: "verified" | "available" | "modified" | "missing";
  favorite?: boolean;
  notes?: string;
}

export interface HistoryFilters {
  model_id?: string;
  voice?: string;
  favorite?: boolean;
  artifact_state?: "available" | "unavailable" | "missing" | "modified";
}

export interface HistoryExportReceipt {
  id: string;
  history_id: string;
  path: string;
  format: "wav" | "flac";
  size_bytes: number;
  sha256: string;
  created_at: string;
}

export interface BenchmarkResult {
  model: string;
  variant: string;
  rtf: number;
  ttfa: number;
  vramGb: number;
  quality: number;
}
