import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  BootstrapState,
  AudioInputDevice,
  AudioOutputDevice,
  AudioPlaybackState,
  AudioRecordingState,
  AudioRecordingOptions,
  DeveloperApiState,
  BatchRunRecord,
  BatchImportResult,
  BatchInputRow,
  ComparisonRecord,
  GenerationPreset,
  GenerationRequest,
  HistoryItem,
  HistoryFilters,
  HistoryExportReceipt,
  JobRecord,
  InstalledModel,
  ModelDownloadProgress,
  ModelInstallPlan,
  ModelIntegrity,
  ModelRuntimeAction,
  MusicGenerationRequest,
  MeasuredBenchmarkRun,
  ProjectRecord,
  ProjectMasterResult,
  ProjectMasterSettings,
  SynthesisRequest,
  SpeakerDiarizationRecord,
  TranscriptionAlignmentRecord,
  TranscriptionRecord,
  VoiceImportRequest,
  VoiceEvaluation,
  VoiceProfile,
  VoiceReferenceEdits,
  EngineHealthState,
  ApplicationSettings,
  SchedulerStatus,
  QueuePriority,
} from "../types";

const PRODUCTION_RUNTIME_ERROR = "The soundAr desktop runtime is unavailable. No preview data was loaded.";

export const hasTauriRuntime = () => {
  const available = "__TAURI_INTERNALS__" in window;
  if (!available && !import.meta.env.DEV) throw new Error(PRODUCTION_RUNTIME_ERROR);
  return available;
};

async function previewBootstrap() {
  if (!import.meta.env.DEV) throw new Error(PRODUCTION_RUNTIME_ERROR);
  return (await import("../data")).fallbackBootstrap;
}
let previewBatches: BatchRunRecord[] = [];
let previewComparisons: ComparisonRecord[] = [];
let previewHistory: HistoryItem[] = [];
const previewHistoryRequests = new Map<string, GenerationRequest>();

export async function loadBootstrapState(): Promise<BootstrapState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 260));
    return previewBootstrap();
  }

  hasTauriRuntime();
  return invoke<BootstrapState>("bootstrap_state");
}

export async function synthesizeSpeech(request: SynthesisRequest): Promise<HistoryItem> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 1200));
    const duration = Math.max(1.8, request.text.length / 13);
    const result: HistoryItem = {
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
      title: request.title ?? request.text.split(/[.!?]/)[0].slice(0, 56) ?? "Untitled generation",
      voice: request.voice_name ?? request.speaker,
      text: request.text,
      generation_kind: "speech",
    };
    previewHistory = [result, ...previewHistory.filter((item) => item.id !== result.id)];
    previewHistoryRequests.set(result.id, { ...request });
    return result;
  }

  return invoke<HistoryItem>("synthesize", { request });
}

export async function queueSynthesis(request: SynthesisRequest): Promise<JobRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Background generation is available in the soundAr desktop app.");
  return invoke<JobRecord>("queue_synthesis", { request });
}

export async function generateMusic(request: MusicGenerationRequest): Promise<HistoryItem> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 1200));
    const aceStep = request.model_id.startsWith("ACE-Step/");
    const result: HistoryItem = {
      id: crypto.randomUUID(),
      model_id: request.model_id,
      engine: aceStep ? "acestep" : "musicgen",
      audio_path: null,
      sample_rate: aceStep ? 48000 : 32000,
      duration_seconds: request.duration_seconds,
      inference_seconds: request.duration_seconds * 0.42,
      rtf: 0.42,
      vram_peak_mb: aceStep ? 10_240 : 6144,
      waveform: Array.from({ length: 96 }, (_, index) => 0.16 + Math.abs(Math.sin(index * 0.37) * Math.cos(index * 0.11)) * 0.78),
      created_at: new Date().toISOString(),
      preview: true,
      title: request.title ?? (request.prompt.slice(0, 56) || "Untitled music draft"),
      voice: "Not applicable",
      text: request.prompt,
      generation_kind: "music",
    };
    previewHistory = [result, ...previewHistory.filter((item) => item.id !== result.id)];
    previewHistoryRequests.set(result.id, { ...request });
    return result;
  }

  return invoke<HistoryItem>("generate_music", { request });
}

export async function queueMusicGeneration(request: MusicGenerationRequest): Promise<JobRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    throw new Error("Background music generation is available in the soundAr desktop app.");
  }
  return invoke<JobRecord>("queue_music_generation", { request });
}

export async function cancelActiveSynthesis(): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("cancel_active_synthesis");
}

export async function listJobs(): Promise<JobRecord[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return [];
  return invoke<JobRecord[]>("list_jobs");
}

export async function getSchedulerStatus(): Promise<SchedulerStatus> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return (await previewBootstrap()).scheduler;
  return invoke<SchedulerStatus>("scheduler_status");
}

export async function cancelJob(jobId: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("cancel_job", { jobId });
}

export async function retryJob(jobId: string): Promise<JobRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Task retry is available in the soundAr desktop app.");
  return invoke<JobRecord>("retry_job", { jobId });
}

export async function clearFinishedJobs(): Promise<number> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return 0;
  return invoke<number>("clear_finished_jobs");
}

export async function saveApplicationSetting<K extends keyof ApplicationSettings>(key: K, value: ApplicationSettings[K]): Promise<ApplicationSettings> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const preview = await previewBootstrap();
    preview.settings = { ...preview.settings, [key]: value };
    return preview.settings;
  }
  return invoke<ApplicationSettings>("save_application_setting", { key, value });
}

export async function listHistory(query = "", filters: HistoryFilters = {}): Promise<HistoryItem[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const search = query.trim().toLowerCase();
    return previewHistory.filter((item) => {
      if (search && ![item.title, item.voice, item.model_id, item.generation_kind, item.text, item.notes].join(" ").toLowerCase().includes(search)) return false;
      if (filters.model_id && item.model_id !== filters.model_id) return false;
      if (filters.voice && item.voice !== filters.voice) return false;
      if (filters.favorite && !item.favorite) return false;
      if (filters.artifact_state === "available" && !item.audio_path) return false;
      if (filters.artifact_state === "unavailable" && item.audio_path) return false;
      return true;
    });
  }
  return invoke<HistoryItem[]>("list_history", { query, filters });
}

export async function duplicateHistoryItem(id: string): Promise<HistoryItem> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("History duplication is available in the soundAr desktop app.");
  return invoke<HistoryItem>("duplicate_history", { id });
}

export async function exportHistoryItem(item: HistoryItem): Promise<HistoryExportReceipt | undefined> {
  if (!hasTauriRuntime() || !item.audio_path) return undefined;
  const extension = item.audio_path.toLowerCase().endsWith(".flac") ? "flac" : "wav";
  const safeTitle = item.title.replace(/[^a-z0-9]+/gi, "-").replace(/^-|-$/g, "").toLowerCase() || "soundar-audio";
  const destination = await save({
    defaultPath: `${safeTitle}.${extension}`,
    filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
  });
  if (!destination) return undefined;
  return invoke<HistoryExportReceipt>("export_history", { id: item.id, destination });
}

export async function deleteHistoryItem(id: string, deleteAudio = true): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const exists = previewHistory.some((item) => item.id === id);
    previewHistory = previewHistory.filter((item) => item.id !== id);
    previewHistoryRequests.delete(id);
    return exists;
  }
  return invoke<boolean>("delete_history", { id, deleteAudio });
}

export async function updateHistoryMetadata(id: string, changes: Pick<Partial<HistoryItem>, "title" | "favorite" | "notes">): Promise<HistoryItem> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const index = previewHistory.findIndex((item) => item.id === id);
    if (index < 0) throw new Error("Preview history item not found.");
    const updated = { ...previewHistory[index], ...changes };
    previewHistory = previewHistory.map((item) => item.id === id ? updated : item);
    return updated;
  }
  return invoke<HistoryItem>("update_history_metadata", { id, changes });
}

export async function getHistoryRequest(id: string): Promise<GenerationRequest> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const request = previewHistoryRequests.get(id);
    if (!request) throw new Error("Preview generation settings not found.");
    return { ...request };
  }
  return invoke<GenerationRequest>("history_request", { id });
}

function batchRows(rows: string[] | BatchInputRow[]): BatchInputRow[] {
  return rows.map((row) => typeof row === "string" ? { text: row } : row);
}

export async function createBatchRun(name: string, rows: string[] | BatchInputRow[], settings: Partial<SynthesisRequest>, priority: QueuePriority = "normal"): Promise<BatchRunRecord> {
  const normalizedRows = batchRows(rows);
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const now = new Date().toISOString();
    const batch: BatchRunRecord = { id: crypto.randomUUID(), name, status: "queued", priority, total_items: normalizedRows.length, completed_items: 0, failed_items: 0, request: { rows: normalizedRows, settings, priority }, items: normalizedRows.map((row, item_index) => ({ id: crypto.randomUUID(), item_index, text: row.text, name: row.name || row.text.split(/[.!?]/)[0].slice(0, 80), output_name: row.output_name || `${String(item_index + 1).padStart(4, "0")}-row`, settings: row.settings ?? {}, priority: row.priority ?? priority, status: "queued", created_at: now, updated_at: now })), created_at: now, updated_at: now };
    previewBatches = [batch, ...previewBatches];
    return batch;
  }
  return invoke<BatchRunRecord>("create_batch", { request: { name, rows: normalizedRows, settings, priority } });
}

export async function queueBatchRun(name: string, rows: string[] | BatchInputRow[], settings: Partial<SynthesisRequest>, parallelism = 2, priority: QueuePriority = "normal"): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return createBatchRun(name, rows, settings, priority);
  return invoke<BatchRunRecord>("queue_batch", { request: { name, rows: batchRows(rows), settings, priority }, parallelism });
}

export async function pickBatchInputFile(): Promise<string | undefined> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return undefined;
  const selected = await open({ multiple: false, directory: false, filters: [{ name: "Batch", extensions: ["txt", "csv", "jsonl"] }] });
  return typeof selected === "string" ? selected : undefined;
}

export async function pickMusicAudioFile(): Promise<string | undefined> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return "/home/theoyinbooke/Music/reference.wav";
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Audio", extensions: ["wav", "flac", "mp3", "m4a", "ogg"] }],
  });
  return typeof selected === "string" ? selected : undefined;
}

export async function importBatchInput(sourcePath: string): Promise<BatchImportResult> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Batch file import is available in the soundAr desktop app.");
  return invoke<BatchImportResult>("import_batch_file", { sourcePath });
}

export async function executeBatchRun(batchId: string, parallelism = 2): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Parallel batch execution is available in the soundAr desktop app.");
  return invoke<BatchRunRecord>("execute_batch", { batchId, parallelism });
}

export async function getBatchRun(batchId: string): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const batch = previewBatches.find((item) => item.id === batchId);
    if (!batch) throw new Error("Preview batch not found.");
    return batch;
  }
  return invoke<BatchRunRecord>("get_batch", { batchId });
}

export async function listBatchRuns(): Promise<BatchRunRecord[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return previewBatches;
  return invoke<BatchRunRecord[]>("list_batches");
}

export async function cancelBatchRun(batchId: string): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const batch = await getBatchRun(batchId);
    const now = new Date().toISOString();
    const updated = { ...batch, status: "cancelled" as const, updated_at: now, items: batch.items.map((item) => ["queued", "running"].includes(item.status) ? { ...item, status: "cancelled" as const, updated_at: now } : item) };
    previewBatches = previewBatches.map((item) => item.id === batchId ? updated : item);
    return updated;
  }
  return invoke<BatchRunRecord>("cancel_batch", { batchId });
}

export async function pauseBatchRun(batchId: string): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const batch = await getBatchRun(batchId);
    const updated = { ...batch, status: "paused" as const, paused: true, updated_at: new Date().toISOString() };
    previewBatches = previewBatches.map((item) => item.id === batchId ? updated : item);
    return updated;
  }
  return invoke<BatchRunRecord>("pause_batch", { batchId });
}

export async function resumeBatchRun(batchId: string, parallelism = 2, retryFailed = false): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const batch = await getBatchRun(batchId);
    const items = retryFailed ? batch.items.map((item) => item.status === "failed" ? { ...item, status: "queued" as const, error: undefined } : item) : batch.items;
    const updated = { ...batch, status: "queued" as const, paused: false, items, failed_items: items.filter((item) => item.status === "failed").length, updated_at: new Date().toISOString() };
    previewBatches = previewBatches.map((item) => item.id === batchId ? updated : item);
    return updated;
  }
  return invoke<BatchRunRecord>("resume_batch", { batchId, parallelism, retryFailed });
}

export async function updateBatchItem(batchId: string, itemIndex: number, status: BatchRunRecord["items"][number]["status"], historyId?: string, error?: string): Promise<BatchRunRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const batch = await getBatchRun(batchId);
    const now = new Date().toISOString();
    const items = batch.items.map((item) => item.item_index === itemIndex ? { ...item, status, history_id: historyId, error, updated_at: now } : item);
    const completed = items.filter((item) => item.status === "completed").length;
    const failed = items.filter((item) => item.status === "failed").length;
    const active = items.some((item) => ["queued", "running"].includes(item.status));
    const batchStatus = active ? (items.some((item) => item.status === "running") ? "running" : "queued") : failed ? "failed" : items.every((item) => item.status === "cancelled") ? "cancelled" : "completed";
    const updated: BatchRunRecord = { ...batch, status: batchStatus, completed_items: completed, failed_items: failed, items, updated_at: now };
    previewBatches = previewBatches.map((item) => item.id === batchId ? updated : item);
    return updated;
  }
  return invoke<BatchRunRecord>("update_batch_item", { batchId, itemIndex, status, historyId, error });
}

export async function saveComparison(comparison: Omit<ComparisonRecord, "id" | "created_at" | "updated_at"> & Partial<Pick<ComparisonRecord, "id">>): Promise<ComparisonRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const now = new Date().toISOString();
    return { ...comparison, id: comparison.id ?? crypto.randomUUID(), created_at: now, updated_at: now } as ComparisonRecord;
  }
  return invoke<ComparisonRecord>("save_comparison", { comparison });
}

export async function createComparison(request: {
  script: string;
  blind: boolean;
  priority?: QueuePriority;
  takes: Partial<SynthesisRequest>[];
}): Promise<ComparisonRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const now = new Date().toISOString();
    const preview = await previewBootstrap();
    const takes = await Promise.all(request.takes.map(async (take, position) => {
      const result = await synthesizeSpeech({
        model_id: take.model_id ?? preview.installed[0]?.model_id ?? "hexgrad/Kokoro-82M",
        text: request.script,
        speaker: take.speaker ?? "default",
        language: take.language ?? "en",
        speed: take.speed ?? 1,
        seed: take.seed ?? 42817 + position,
        output_format: take.output_format ?? "wav",
        priority: take.priority ?? request.priority,
        title: `Compare ${String.fromCharCode(65 + position)}: ${request.script.slice(0, 36)}`,
        voice_name: take.voice_name ?? "Comparison voice",
      });
      return { id: crypto.randomUUID(), position, label: String.fromCharCode(65 + position), request: { ...take, text: request.script } as SynthesisRequest, status: "completed" as const, notes: "", favorite: false, result, history_id: result.id, created_at: now, updated_at: now };
    }));
    const run: ComparisonRecord = { id: crypto.randomUUID(), script: request.script, status: "completed", blind: request.blind, revealed: !request.blind, tie: false, notes: "", takes, created_at: now, updated_at: now };
    previewComparisons = [run, ...previewComparisons];
    return run;
  }
  return invoke<ComparisonRecord>("create_comparison", { request });
}

export async function getComparison(comparisonId: string): Promise<ComparisonRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const run = previewComparisons.find((comparison) => comparison.id === comparisonId);
    if (!run) throw new Error("The preview comparison was not found.");
    return run;
  }
  return invoke<ComparisonRecord>("get_comparison", { comparisonId });
}

export async function updateComparisonReview(comparisonId: string, changes: Record<string, unknown>): Promise<ComparisonRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const run = await getComparison(comparisonId);
    const takeId = typeof changes.take_id === "string" ? changes.take_id : undefined;
    const takes = run.takes.map((take) => take.id === takeId ? {
      ...take,
      rating: typeof changes.rating === "number" ? changes.rating : take.rating,
      notes: typeof changes.notes === "string" ? changes.notes : take.notes,
      favorite: typeof changes.favorite === "boolean" ? changes.favorite : take.favorite,
      updated_at: new Date().toISOString(),
    } : changes.promoted_take_id === take.id && take.result ? { ...take, result: { ...take.result, favorite: true } } : take);
    const updated: ComparisonRecord = {
      ...run,
      takes,
      revealed: typeof changes.revealed === "boolean" ? changes.revealed : run.revealed,
      tie: typeof changes.tie === "boolean" ? changes.tie : typeof changes.winner_take_id === "string" ? false : run.tie,
      winner_take_id: typeof changes.winner_take_id === "string" ? changes.winner_take_id : run.winner_take_id,
      promoted_take_id: typeof changes.promoted_take_id === "string" ? changes.promoted_take_id : run.promoted_take_id,
      notes: typeof changes.notes === "string" && !takeId ? changes.notes : run.notes,
      updated_at: new Date().toISOString(),
    };
    previewComparisons = previewComparisons.map((comparison) => comparison.id === comparisonId ? updated : comparison);
    return updated;
  }
  return invoke<ComparisonRecord>("update_comparison_review", { comparisonId, changes });
}

export async function cancelComparison(comparisonId: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const run = await getComparison(comparisonId);
    const updated = { ...run, status: "cancelled" as const, takes: run.takes.map((take) => take.status === "completed" ? take : { ...take, status: "cancelled" as const }) };
    previewComparisons = previewComparisons.map((comparison) => comparison.id === comparisonId ? updated : comparison);
    return true;
  }
  return invoke<boolean>("cancel_comparison", { comparisonId });
}

export async function saveGenerationPreset(
  name: string,
  settings: Partial<SynthesisRequest>,
): Promise<GenerationPreset> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    return { id: crypto.randomUUID(), name, schema_version: 1, settings, created_at: new Date().toISOString() };
  }
  return invoke<GenerationPreset>("save_preset", { preset: { name, settings } });
}

export async function saveBenchmarkRun(result: { history_id: string; transcription_id: string } & Partial<Pick<MeasuredBenchmarkRun, "model_revision" | "gpu_name" | "driver_version" | "app_version">>): Promise<MeasuredBenchmarkRun> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Measured benchmark evidence is available in the soundAr desktop app.");
  return invoke<MeasuredBenchmarkRun>("save_benchmark", { result });
}

export async function prepareBenchmarkEngine(modelId: string): Promise<{ engine: string; retired_workers: number; ready: boolean; token: string }> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Cold-start benchmarks are available in the soundAr desktop app.");
  return invoke("prepare_benchmark_engine", { modelId });
}

export async function releaseBenchmarkEngine(token: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke("release_benchmark_engine", { token });
}

export async function importVoiceProfile(request: VoiceImportRequest): Promise<VoiceProfile> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Voice import is available in the soundAr desktop app.");
  return invoke<VoiceProfile>("create_voice", { request });
}

export async function addVoiceReference(voiceId: string, sourcePath: string): Promise<VoiceProfile> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Voice references are available in the soundAr desktop app.");
  return invoke<VoiceProfile>("add_voice_reference", { voiceId, sourcePath });
}

export async function processVoiceReference(voiceId: string, referenceId: string, edits: VoiceReferenceEdits): Promise<VoiceProfile> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Reference editing is available in the soundAr desktop app.");
  return invoke<VoiceProfile>("process_voice_reference", { voiceId, referenceId, edits });
}

export async function updateVoiceReferenceTranscript(voiceId: string, referenceId: string, transcript: string, source: "automatic" | "corrected" | "none"): Promise<VoiceProfile> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const voice = (await previewBootstrap()).voices.find((item) => item.id === voiceId);
    if (!voice) throw new Error("Preview voice was not found.");
    return { ...voice, references: voice.references?.map((item) => item.id === referenceId ? { ...item, transcript_text: transcript.trim(), transcript_source: source } : item) };
  }
  return invoke<VoiceProfile>("update_voice_reference_transcript", { voiceId, referenceId, transcript, source });
}

export async function saveVoiceEvaluation(evaluation: Omit<VoiceEvaluation, "created_at" | "updated_at">): Promise<VoiceEvaluation> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const now = new Date().toISOString();
    return { ...evaluation, created_at: now, updated_at: now };
  }
  return invoke<VoiceEvaluation>("save_voice_evaluation", { evaluation });
}

export async function measureVoiceSimilarity(evaluationId: string, modelId: string): Promise<VoiceEvaluation> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Speaker similarity is available in the soundAr desktop app.");
  return invoke<VoiceEvaluation>("measure_voice_similarity", { evaluationId, modelId });
}

export async function deleteVoiceProfile(id: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("delete_voice", { id });
}

export async function saveProject(project: Partial<ProjectRecord> & { name: string }): Promise<ProjectRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const now = new Date().toISOString();
    return {
      id: project.id ?? crypto.randomUUID(),
      name: project.name,
      document: project.document ?? { script: "", chapters: [], speaker_assignments: {} },
      created_at: project.created_at ?? now,
      updated_at: now,
    };
  }
  return invoke<ProjectRecord>("save_project", { project });
}

export async function listProjects(): Promise<ProjectRecord[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return (await previewBootstrap()).projects;
  return invoke<ProjectRecord[]>("list_projects");
}

export async function deleteProject(id: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("delete_project", { id });
}

export async function pickProjectScript(): Promise<string | undefined> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return undefined;
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Script", extensions: ["txt", "md", "markdown", "csv", "jsonl", "srt"] }],
  });
  return typeof selected === "string" ? selected : undefined;
}

export async function importProjectScript(sourcePath: string): Promise<ProjectRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Project import is available in the soundAr desktop app.");
  return invoke<ProjectRecord>("import_project_script", { sourcePath });
}

export async function exportProjectMaster(projectId: string, settings: ProjectMasterSettings): Promise<ProjectMasterResult> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Project mastering is available in the soundAr desktop app.");
  return invoke<ProjectMasterResult>("export_project_master", { projectId, settings });
}

export async function transcribeAudio(modelId: string, audioPath: string, cleanup = false): Promise<TranscriptionRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Transcription is available in the soundAr desktop app.");
  return invoke<TranscriptionRecord>("transcribe_audio", { modelId, audioPath, cleanup });
}

export async function updateTranscription(
  transcriptionId: string,
  text: string,
  segments: TranscriptionRecord["segments"],
): Promise<Pick<TranscriptionRecord, "id" | "text" | "segments" | "revision_count" | "updated_at">> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    return { id: transcriptionId, text: text.trim(), segments, revision_count: 1, updated_at: new Date().toISOString() };
  }
  return invoke("update_transcription", { transcriptionId, text, segments });
}

export async function diarizeTranscription(
  transcriptionId: string,
  modelId: string,
  speakerCount?: number,
): Promise<SpeakerDiarizationRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    const transcription = (await previewBootstrap()).transcriptions.find((record) => record.id === transcriptionId);
    if (!transcription) throw new Error("The preview transcript was not found.");
    const split = Math.max(1, Math.floor(transcription.words.length / 2));
    const groups = [transcription.words.slice(0, split), transcription.words.slice(split)].filter((group) => group.length);
    const now = new Date().toISOString();
    const diarization: SpeakerDiarizationRecord = {
      id: crypto.randomUUID(),
      job_id: crypto.randomUUID(),
      model_id: modelId,
      engine: "speaker-verification",
      speakers: groups.map((_, index) => ({ id: `speaker-${index + 1}`, default_name: `Speaker ${index + 1}` })),
      turns: groups.map((group, index) => ({
        speaker_id: `speaker-${index + 1}`,
        start_seconds: group[0].start_seconds,
        end_seconds: group.at(-1)?.end_seconds ?? group[0].end_seconds,
        word_start_index: index === 0 ? 0 : split,
        word_end_index: index === 0 ? split - 1 : transcription.words.length - 1,
        text: group.map((word) => word.text).join(" "),
        confidence: null,
      })),
      evidence: {
        schema_version: 1,
        method: "wavlm-xvector-word-window-clustering",
        clustering: "average-link-cosine",
        distance_threshold: 0.32,
        speaker_count_mode: speakerCount ? "fixed" : "automatic",
        requested_speaker_count: speakerCount ?? null,
        speech_window_count: groups.length,
        overlap_detection: false,
        confidence_source: "unavailable",
        provisional: true,
      },
      inference_seconds: 0.24,
      vram_peak_mb: 620,
      labels: Object.fromEntries(groups.map((_, index) => [`speaker-${index + 1}`, `Speaker ${index + 1}`])),
      label_revision_count: 0,
      labels_updated_at: null,
      created_at: now,
    };
    transcription.diarization = diarization;
    return diarization;
  }
  return invoke<SpeakerDiarizationRecord>("diarize_transcription", { transcriptionId, modelId, speakerCount });
}

export async function alignTranscription(
  transcriptionId: string,
  modelId: string,
): Promise<TranscriptionAlignmentRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    await new Promise((resolve) => window.setTimeout(resolve, 350));
    const transcription = (await previewBootstrap()).transcriptions.find((record) => record.id === transcriptionId);
    if (!transcription) throw new Error("The preview transcript was not found.");
    const now = new Date().toISOString();
    const alignment: TranscriptionAlignmentRecord = {
      id: crypto.randomUUID(),
      job_id: crypto.randomUUID(),
      model_id: modelId,
      engine: "alignment",
      source_revision: transcription.revision_count ?? 0,
      source_text_sha256: "browser-preview",
      words: transcription.words.map((word, index) => ({
        text: word.text,
        start_seconds: word.start_seconds,
        end_seconds: word.end_seconds,
        alignment_score: Number((0.91 - (index % 4) * 0.03).toFixed(2)),
        segment_index: 0,
      })),
      evidence: {
        schema_version: 1,
        method: "browser-preview",
        language: "en",
        source_revision_linked: true,
        score_source: "preview-only",
        score_calibrated: false,
        original_timestamps_preserved: true,
        provisional: true,
      },
      mean_alignment_score: 0.87,
      inference_seconds: 0.18,
      vram_peak_mb: 512,
      current: true,
      created_at: now,
    };
    transcription.alignment = alignment;
    return alignment;
  }
  return invoke<TranscriptionAlignmentRecord>("align_transcription", { transcriptionId, modelId });
}

export async function updateTranscriptionSpeakerLabels(
  transcriptionId: string,
  labels: Record<string, string>,
): Promise<Pick<SpeakerDiarizationRecord, "labels" | "label_revision_count" | "labels_updated_at">> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const transcription = (await previewBootstrap()).transcriptions.find((record) => record.id === transcriptionId);
    if (!transcription?.diarization) throw new Error("Run speaker separation before editing labels.");
    transcription.diarization.labels = { ...labels };
    transcription.diarization.label_revision_count += 1;
    transcription.diarization.labels_updated_at = new Date().toISOString();
    return {
      labels: transcription.diarization.labels,
      label_revision_count: transcription.diarization.label_revision_count,
      labels_updated_at: transcription.diarization.labels_updated_at,
    };
  }
  return invoke("update_transcription_speaker_labels", { transcriptionId, labels });
}

export async function setupPythonRuntime(onProgress: (message: string) => void): Promise<void> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return;
  const unlisten = await listen<string>("runtime-setup-progress", (event) => onProgress(event.payload));
  try {
    await invoke("setup_runtime");
  } finally {
    unlisten();
  }
}

export async function setupEngineRuntime(engine: string, onProgress: (message: string) => void): Promise<void> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return;
  const unlisten = await listen<{ engine: string; message: string }>("engine-runtime-progress", (event) => {
    if (event.payload.engine === engine) onProgress(event.payload.message);
  });
  try {
    await invoke("setup_engine_runtime", { engine });
  } finally {
    unlisten();
  }
}

export async function getEngineHealth(engine: string): Promise<EngineHealthState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { status: "preview", device: "browser", engine_scope: engine, engine_runtime: "preview", process_id: 0, warm_workers: 0, worker_starts: 0, worker_restarts: 0, worker_failures: 0, loaded_models: [] };
  return invoke("engine_health", { engine });
}

export async function queueModelRuntimeLoad(modelId: string): Promise<JobRecord> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Model loading is available in the soundAr desktop app.");
  return invoke<JobRecord>("queue_model_runtime_load", { modelId });
}

export async function unloadModelRuntime(modelId: string): Promise<ModelRuntimeAction> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Model unloading is available in the soundAr desktop app.");
  return invoke<ModelRuntimeAction>("unload_model_runtime", { modelId });
}

export async function getDeveloperApiStatus(): Promise<DeveloperApiState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { running: false };
  return invoke<DeveloperApiState>("developer_api_status");
}

export async function startDeveloperApi(port = 17843): Promise<DeveloperApiState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("The developer API is available in the soundAr desktop app.");
  return invoke<DeveloperApiState>("start_developer_api", { port });
}

export async function stopDeveloperApi(): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("stop_developer_api");
}

export async function listAudioInputDevices(): Promise<AudioInputDevice[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return [];
  return invoke<AudioInputDevice[]>("list_audio_input_devices");
}

export async function listAudioOutputDevices(): Promise<AudioOutputDevice[]> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return [];
  return invoke<AudioOutputDevice[]>("list_audio_output_devices");
}

export async function startAudioPlayback(audioPath: string, deviceId?: string): Promise<AudioPlaybackState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Routed playback is available in the soundAr desktop app.");
  return invoke<AudioPlaybackState>("start_audio_playback", { audioPath, deviceId });
}

export async function getAudioPlaybackStatus(): Promise<AudioPlaybackState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { playing: false };
  return invoke<AudioPlaybackState>("audio_playback_status");
}

export async function stopAudioPlayback(): Promise<AudioPlaybackState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { playing: false };
  return invoke<AudioPlaybackState>("stop_audio_playback");
}

export async function startAudioRecording(options: AudioRecordingOptions): Promise<AudioRecordingState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Microphone capture is available in the soundAr desktop app.");
  return invoke<AudioRecordingState>("start_audio_recording", {
    deviceId: options.device_id,
    vadEnabled: options.vad_enabled,
    autoStop: options.auto_stop,
    silenceMs: options.silence_ms,
    inputGain: options.input_gain,
  });
}

export async function getAudioRecordingStatus(): Promise<AudioRecordingState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { recording: false };
  return invoke<AudioRecordingState>("audio_recording_status");
}

export async function stopAudioRecording(): Promise<AudioRecordingState> {
  if (import.meta.env.DEV && !hasTauriRuntime()) throw new Error("Microphone capture is available in the soundAr desktop app.");
  return invoke<AudioRecordingState>("stop_audio_recording");
}

export async function getModelInstallPlan(modelId: string, revision?: string): Promise<ModelInstallPlan> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    throw new Error("Model installation is available in the soundAr desktop app.");
  }
  return invoke<ModelInstallPlan>("model_install_plan", { modelId, revision });
}

export async function verifyModel(modelId: string): Promise<ModelIntegrity> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    const installed = (await previewBootstrap()).installed.find((model) => model.model_id === modelId);
    return installed?.integrity ?? {
      model_id: modelId,
      state: installed ? "ready" : "not-installed",
      reason: installed ? "verified" : "not-installed",
      missing_files: [],
      invalid_files: [],
      checked_files: 0,
      installed_size_bytes: installed?.installed_size_bytes ?? 0,
      manifest_verified: false,
    };
  }
  return invoke<ModelIntegrity>("verify_model", { modelId });
}

export async function installModel(
  plan: ModelInstallPlan,
  onProgress: (progress: ModelDownloadProgress) => void,
): Promise<InstalledModel> {
  if (import.meta.env.DEV && !hasTauriRuntime()) {
    throw new Error("Model installation is available in the soundAr desktop app.");
  }
  const unlisten = await listen<ModelDownloadProgress>("model-download-progress", (event) => {
    if (event.payload.model_id === plan.model_id) onProgress(event.payload);
  });
  try {
    return await invoke<InstalledModel>("install_model", {
      modelId: plan.model_id,
      revision: plan.revision,
    });
  } finally {
    unlisten();
  }
}

export async function cancelModelInstall(modelId: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("cancel_model_install", { modelId });
}

export async function removeModel(modelId: string): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return false;
  return invoke<boolean>("remove_model", { modelId });
}

export async function loadGeneratedAudio(path: string): Promise<string> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return path;
  const format = path.toLowerCase().endsWith(".flac") ? "flac" : "wav";
  const payload = await invoke<ArrayBuffer | Uint8Array | number[]>("read_generated_audio", { path });
  const bytes = payload instanceof ArrayBuffer ? new Uint8Array(payload) : Uint8Array.from(payload);
  const expectedHeader = format === "flac" ? "fLaC" : "RIFF";
  const header = String.fromCharCode(...bytes.subarray(0, 4));
  if (header !== expectedHeader) {
    throw new Error(`Generated ${format.toUpperCase()} data has an invalid header.`);
  }
  return URL.createObjectURL(new Blob([bytes], { type: format === "flac" ? "audio/flac" : "audio/wav" }));
}

export async function loadTranscriptionAudio(path: string): Promise<string> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return path;
  const payload = await invoke<ArrayBuffer | Uint8Array | number[]>("read_transcription_audio", { path });
  const bytes = payload instanceof ArrayBuffer ? new Uint8Array(payload) : Uint8Array.from(payload);
  const extension = path.toLowerCase().split(".").at(-1) ?? "wav";
  const mime = ({ wav: "audio/wav", flac: "audio/flac", mp3: "audio/mpeg", m4a: "audio/mp4", ogg: "audio/ogg" } as Record<string, string>)[extension] ?? "application/octet-stream";
  return URL.createObjectURL(new Blob([bytes], { type: mime }));
}

export async function loadVoiceAudio(path: string): Promise<string> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return path;
  const payload = await invoke<ArrayBuffer | Uint8Array | number[]>("read_voice_audio", { path });
  const bytes = payload instanceof ArrayBuffer ? new Uint8Array(payload) : Uint8Array.from(payload);
  const extension = path.toLowerCase().split(".").at(-1) ?? "wav";
  const mime = ({ wav: "audio/wav", flac: "audio/flac", mp3: "audio/mpeg", m4a: "audio/mp4", ogg: "audio/ogg" } as Record<string, string>)[extension] ?? "application/octet-stream";
  return URL.createObjectURL(new Blob([bytes], { type: mime }));
}

export function isDesktopRuntime(): boolean {
  return hasTauriRuntime();
}

export async function pickAudioFile(): Promise<string | undefined> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return undefined;
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{ name: "Audio", extensions: ["wav", "flac", "mp3", "ogg", "m4a"] }],
  });
  return typeof selected === "string" ? selected : undefined;
}
