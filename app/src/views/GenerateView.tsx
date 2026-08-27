import { ChevronDown, ChevronRight, FileInput, FolderOpen, Info, Pause, Play, Plus, RotateCcw, Save, Trash2, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { cancelBatchRun, cancelJob, clearFinishedJobs, createBatchRun, deleteHistoryItem, getSchedulerStatus, importBatchInput, listBatchRuns, listHistory, listJobs, loadGeneratedAudio, pauseBatchRun, pickBatchInputFile, queueBatchRun, queueSynthesis, resumeBatchRun, retryJob, saveGenerationPreset, synthesizeSpeech, updateBatchItem } from "../lib/bridge";
import { capabilityForModel, compatibleVoicesForModel, qualifiedModels, recommendModel } from "../lib/capabilities";
import type { BatchImportResult, BatchInputRow, BatchRunRecord, BootstrapState, HistoryItem, JobRecord, QueuePriority, RouteIntent, SynthesisRequest, SynthesisResult, VoiceProfile } from "../types";
import { CompactField, Dropdown, MetricStrip, PageHeader, Panel, RowActionMenu, Segmented, SelectField, StatusText } from "../components/ui";
import { VoiceProfileDialog } from "../components/VoiceProfileDialog";
import { MusicGeneratePanel } from "./MusicGeneratePanel";

function formatPlaybackTime(seconds: number) {
  if (!Number.isFinite(seconds)) return "0:00.0";
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds % 60).toFixed(1).padStart(4, "0")}`;
}

function jobFailureSummary(error?: string | null) {
  if (!error) return undefined;
  const normalized = error.toLowerCase();
  if (normalized.includes("out of memory") || normalized.includes("cuda oom")) return "Not enough free GPU memory";
  return error.split(/\r?\n/)[0].slice(0, 96);
}

export function GenerateView({
  bootstrap,
  voices,
  onVoicesChange,
  onGenerated,
  preferredVoiceId,
}: {
  bootstrap: BootstrapState;
  voices: VoiceProfile[];
  onVoicesChange: (voices: VoiceProfile[]) => void;
  onGenerated: (item: HistoryItem) => void;
  preferredVoiceId?: string;
}) {
  const ttsModels = useMemo(() => qualifiedModels(bootstrap, "tts"), [bootstrap]);
  const [generationKind, setGenerationKind] = useState<"speech" | "music">("speech");
  const [mode, setMode] = useState<"text" | "ssml" | "batch">("text");
  const [text, setText] = useState("The best voices feel present before they sound perfect.\nStart with clarity, then shape pace, warmth, and intent.");
  const [modelId, setModelId] = useState(ttsModels.find((model) => model.engine === "kokoro")?.model_id ?? ttsModels[0]?.model_id ?? "");
  const [voiceId, setVoiceId] = useState(voices[0]?.id ?? "");
  const [speed, setSpeed] = useState(1);
  const [exaggeration, setExaggeration] = useState(0.5);
  const [cfgWeight, setCfgWeight] = useState(0.5);
  const [temperature, setTemperature] = useState(0.8);
  const [topP, setTopP] = useState(0.95);
  const [repetitionPenalty, setRepetitionPenalty] = useState(1.2);
  const [parallelism, setParallelism] = useState(Math.min(2, bootstrap.scheduler.max_workers));
  const [priority, setPriority] = useState<QueuePriority>("normal");
  const [batchImport, setBatchImport] = useState<BatchImportResult>();
  const [language, setLanguage] = useState("en");
  const [seed, setSeed] = useState(42817);
  const [outputFormat, setOutputFormat] = useState<"wav" | "flac">("wav");
  const [result, setResult] = useState<SynthesisResult>();
  const [isGenerating, setIsGenerating] = useState(false);
  const [batchProgress, setBatchProgress] = useState<{ completed: number; total: number; failed: number }>();
  const [error, setError] = useState<string>();
  const [playbackError, setPlaybackError] = useState<string>();
  const [isPlaying, setIsPlaying] = useState(false);
  const [audioUrl, setAudioUrl] = useState<string>();
  const [isAudioLoading, setIsAudioLoading] = useState(false);
  const [playbackTime, setPlaybackTime] = useState(0);
  const [playbackDuration, setPlaybackDuration] = useState(0);
  const [presetState, setPresetState] = useState<string>();
  const [presets, setPresets] = useState(bootstrap.presets);
  const [selectedPresetId, setSelectedPresetId] = useState("");
  const [routeIntent, setRouteIntent] = useState<RouteIntent>("manual");
  const [routeReason, setRouteReason] = useState<string>();
  const [batches, setBatches] = useState(bootstrap.batches);
  const [expandedBatchId, setExpandedBatchId] = useState<string>();
  const [jobs, setJobs] = useState<JobRecord[]>(bootstrap.jobs);
  const [scheduler, setScheduler] = useState(bootstrap.scheduler);
  const [showAddVoice, setShowAddVoice] = useState(false);
  const [recentOutputs, setRecentOutputs] = useState<HistoryItem[]>([]);
  const [recentPlayingId, setRecentPlayingId] = useState<string>();
  const [activityStage, setActivityStage] = useState<"queued" | "progress" | "completed">("progress");
  const audioRef = useRef<HTMLAudioElement>(null);
  const recentAudioRef = useRef<HTMLAudioElement>(null);
  const cancelRequested = useRef(false);
  const submittedJobIds = useRef(new Set<string>());
  const deliveredHistoryIds = useRef(new Set<string>());
  const pendingCreatedVoiceId = useRef<string | undefined>(undefined);
  const selectedModel = ttsModels.find((model) => model.model_id === modelId);
  const capability = capabilityForModel(bootstrap, selectedModel);
  const libraryVoices = compatibleVoicesForModel(bootstrap, selectedModel, voices);
  const supportsEngineDefault = capability?.voice_modes.includes("default") === true;
  const compatibleVoices = supportsEngineDefault
    ? [{ id: "__engine_default__", name: `${capability.display_name} default`, style: "Built-in engine voice", sample_label: "Engine default", sample_seconds: 0, engines: [capability.display_name], consent: "not-required" as const, state: "preset" as const, color: "amber" as const }, ...libraryVoices]
    : libraryVoices;
  const selectedVoice = compatibleVoices.find((voice) => voice.id === voiceId);
  const referenceRequired = capability?.voice_modes.length === 1 && capability.voice_modes[0] === "reference";
  const voiceReady = !referenceRequired || Boolean(selectedVoice?.local_path);

  function useCreatedVoice(voice: VoiceProfile) {
    pendingCreatedVoiceId.current = voice.id;
    onVoicesChange([...voices, voice]);
    const currentModelSupportsVoice = selectedModel
      ? compatibleVoicesForModel(bootstrap, selectedModel, [voice]).length > 0
      : false;
    const compatibleModel = currentModelSupportsVoice
      ? selectedModel
      : ttsModels.find((model) => compatibleVoicesForModel(bootstrap, model, [voice]).length > 0);
    if (compatibleModel) setModelId(compatibleModel.model_id);
    setVoiceId(voice.id);
    setShowAddVoice(false);
  }

  function applyRoute(intent: RouteIntent) {
    setRouteIntent(intent);
    if (intent === "manual") {
      setRouteReason(undefined);
      return;
    }
    const recommendation = recommendModel(bootstrap, intent, voices);
    setRouteReason(recommendation.reason);
    if (recommendation.model) setModelId(recommendation.model.model_id);
  }

  useEffect(() => {
    if (!modelId && ttsModels[0]) setModelId(ttsModels[0].model_id);
  }, [modelId, ttsModels]);

  useEffect(() => {
    if (preferredVoiceId && voices.some((voice) => voice.id === preferredVoiceId)) {
      const preferred = voices.find((voice) => voice.id === preferredVoiceId);
      const matchingModel = ttsModels.find((model) => preferred?.engines.some((engine) => engine.toLowerCase() === (model.engine === "coqui" ? "xtts" : model.engine)));
      if (matchingModel) setModelId(matchingModel.model_id);
      setVoiceId(preferredVoiceId);
    }
  }, [preferredVoiceId]);

  useEffect(() => {
    const model = ttsModels.find((entry) => entry.model_id === modelId);
    const nextCapability = capabilityForModel(bootstrap, model);
    if (nextCapability && !nextCapability.languages.includes(language)) setLanguage(nextCapability.languages[0] ?? "en");
    const nextLibraryVoices = compatibleVoicesForModel(bootstrap, model, voices);
    const nextVoices = nextCapability?.voice_modes.includes("default")
      ? ["__engine_default__", ...nextLibraryVoices.map((voice) => voice.id)]
      : nextLibraryVoices.map((voice) => voice.id);
    if (pendingCreatedVoiceId.current && nextVoices.includes(pendingCreatedVoiceId.current)) {
      setVoiceId(pendingCreatedVoiceId.current);
      pendingCreatedVoiceId.current = undefined;
    } else if (!nextVoices.includes(voiceId)) {
      setVoiceId(nextVoices[0] ?? "");
    }
  }, [bootstrap, modelId, voices, voiceId]);

  useEffect(() => {
    let active = true;
    let objectUrl: string | undefined;
    setAudioUrl(undefined);
    setPlaybackError(undefined);
    setIsPlaying(false);
    setPlaybackTime(0);
    setPlaybackDuration(result?.duration_seconds ?? 0);
    if (!result?.audio_path) return;

    setIsAudioLoading(true);
    loadGeneratedAudio(result.audio_path)
      .then((url) => {
        objectUrl = url;
        if (active) setAudioUrl(url);
        else if (url.startsWith("blob:")) URL.revokeObjectURL(url);
      })
      .catch((caught) => {
        if (active) setPlaybackError(caught instanceof Error ? caught.message : String(caught));
      })
      .finally(() => { if (active) setIsAudioLoading(false); });

    return () => {
      active = false;
      if (objectUrl?.startsWith("blob:")) URL.revokeObjectURL(objectUrl);
    };
  }, [result?.audio_path]);

  useEffect(() => {
    if (bootstrap.runtime !== "tauri") return;
    let active = true;
    async function refreshQueue() {
      try {
        const [nextJobs, nextHistory, nextBatches, nextScheduler] = await Promise.all([listJobs(), listHistory(), listBatchRuns(), getSchedulerStatus()]);
        if (!active) return;
        setJobs(nextJobs);
        setBatches(nextBatches);
        setScheduler(nextScheduler);
        setRecentOutputs(nextHistory.slice(0, 6));
        nextBatches.flatMap((batch) => batch.items).forEach((item) => { if (item.job_id) submittedJobIds.current.add(item.job_id); });
        const activeIds = new Set(nextJobs.filter((job) => ["queued", "preparing", "running"].includes(job.status)).map((job) => job.id));
        setIsGenerating(activeIds.size > 0);
        const activeBatch = nextBatches.find((batch) => ["queued", "running"].includes(batch.status));
        setBatchProgress(activeBatch ? { completed: activeBatch.completed_items + activeBatch.failed_items, total: activeBatch.total_items, failed: activeBatch.failed_items } : undefined);
        for (const item of nextHistory) {
          if (!deliveredHistoryIds.current.has(item.id) && item.job_id && submittedJobIds.current.has(item.job_id)) {
            deliveredHistoryIds.current.add(item.id);
            onGenerated(item);
            setResult(item);
          }
        }
      } catch {
        // The next poll retries transient shutdown or migration races.
      }
    }
    void refreshQueue();
    const timer = window.setInterval(() => void refreshQueue(), 500);
    return () => { active = false; window.clearInterval(timer); };
  }, [bootstrap.runtime]);

  useEffect(() => {
    if (bootstrap.runtime === "tauri") return;
    void listHistory().then((items) => setRecentOutputs(items.slice(0, 6))).catch(() => undefined);
  }, [bootstrap.runtime]);

  useEffect(() => {
    if (generationKind !== "speech") return;
    function handleShortcut(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey)) return;
      if (event.key === "Enter") {
        event.preventDefault();
        void generate();
      }
      if (event.key.toLowerCase() === "p" && audioUrl) {
        event.preventDefault();
        void togglePlayback();
      }
    }
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [audioUrl, generationKind, isGenerating, isPlaying, modelId, mode, outputFormat, seed, speed, text, voiceId]);
  const runtimeReady = bootstrap.runtime === "browser" || bootstrap.system.python_ready;
  const estimatedSeconds = Math.max(2, Math.round(text.length / 13));

  async function importBatch() {
    setError(undefined);
    try {
      const sourcePath = await pickBatchInputFile();
      if (!sourcePath) return;
      const imported = await importBatchInput(sourcePath);
      setBatchImport(imported);
      setText(imported.rows.map((row) => row.text).join("\n"));
    } catch (caught) {
      setBatchImport(undefined);
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function generate() {
    if (!runtimeReady || !text.trim() || !modelId || !voiceReady) return;
    if (jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status)).length >= scheduler.max_workers) {
      setError(`All ${scheduler.max_workers} generation slots are in use. Wait for one to finish before starting another.`);
      return;
    }
    cancelRequested.current = false;
    setError(undefined);
    const rows: BatchInputRow[] = mode === "batch"
      ? batchImport?.rows ?? text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line) => ({ text: line }))
      : [{ text: text.trim() }];
    const scripts = rows.map((row) => row.text);
    setBatchProgress(mode === "batch" ? { completed: 0, total: scripts.length, failed: 0 } : undefined);
    try {
      const settings: Partial<SynthesisRequest> = {
        model_id: modelId,
        input_mode: mode === "ssml" ? "ssml" : "text",
        speaker: selectedModel?.engine === "kokoro" ? selectedVoice?.id ?? "af_heart" : "default",
        language,
        reference_audio_path: selectedVoice?.state === "ready" ? selectedVoice.local_path : undefined,
        speed,
        exaggeration,
        cfg_weight: cfgWeight,
        temperature,
        top_p: topP,
        repetition_penalty: repetitionPenalty,
        seed,
        output_format: outputFormat,
        voice_name: selectedVoice?.name ?? "Default voice",
        priority,
      };
      let batch: BatchRunRecord | undefined;
      if (mode === "batch") {
        batch = bootstrap.runtime === "tauri"
          ? await queueBatchRun(batchImport?.name || `Batch ${new Date().toLocaleString()}`, rows, settings, parallelism, priority)
          : await createBatchRun(batchImport?.name || `Batch ${new Date().toLocaleString()}`, rows, settings, priority);
        setBatches((items) => [batch!, ...items]);
        if (bootstrap.runtime === "tauri") {
          setBatchProgress({ completed: 0, total: batch.total_items, failed: 0 });
          return;
        }
      }
      if (bootstrap.runtime === "tauri") {
        const request: SynthesisRequest = {
          model_id: settings.model_id ?? modelId,
          text: scripts[0],
          input_mode: settings.input_mode,
          speaker: settings.speaker ?? "default",
          language: settings.language ?? "en",
          reference_audio_path: settings.reference_audio_path,
          speed: settings.speed ?? 1,
          exaggeration: settings.exaggeration,
          cfg_weight: settings.cfg_weight,
          temperature: settings.temperature,
          top_p: settings.top_p,
          repetition_penalty: settings.repetition_penalty,
          seed: settings.seed ?? seed,
          output_format: settings.output_format ?? "wav",
          title: scripts[0].split(/[.!?]/)[0].slice(0, 56),
          voice_name: settings.voice_name,
          priority,
        };
        const queued = await queueSynthesis(request);
        submittedJobIds.current.add(queued.id);
        setJobs((items) => [queued, ...items]);
        return;
      }
      setIsGenerating(true);
      const { latest, failed } = await runScripts(scripts.map((line, itemIndex) => ({ line, itemIndex })), settings, batch?.id);
      if (latest) {
        setResult(latest);
        setRecentOutputs((items) => [latest, ...items.filter((item) => item.id !== latest.id)].slice(0, 6));
        setPlaybackError(undefined);
        setIsPlaying(false);
      }
      if (failed) setError(`${failed} of ${scripts.length} batch rows failed. Successful rows are preserved in History.`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setIsGenerating(false);
    }
  }

  async function retryQueuedJob(job: JobRecord) {
    setError(undefined);
    try {
      const retried = await retryJob(job.id);
      submittedJobIds.current.add(job.id);
      setJobs((items) => items.map((item) => item.id === job.id ? retried : item));
      setIsGenerating(true);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function clearFinishedQueue() {
    try {
      await clearFinishedJobs();
      setJobs((items) => items.filter((job) => !["completed", "failed", "cancelled"].includes(job.status)));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function runScripts(
    rows: Array<{ line: string; itemIndex: number }>,
    settings: Partial<SynthesisRequest>,
    batchId?: string,
  ): Promise<{ latest?: HistoryItem; failed: number }> {
    let latest: HistoryItem | undefined;
    let failed = 0;
    for (let position = 0; position < rows.length; position += 1) {
      const { line, itemIndex } = rows[position];
      if (batchId) {
        const nextBatch = await updateBatchItem(batchId, itemIndex, "running");
        setBatches((items) => [nextBatch, ...items.filter((item) => item.id !== nextBatch.id)]);
      }
      try {
        const next = await synthesizeSpeech({
          model_id: settings.model_id ?? modelId,
          text: line,
          input_mode: settings.input_mode ?? "text",
          speaker: settings.speaker ?? "default",
          language: settings.language ?? "en",
          reference_audio_path: settings.reference_audio_path,
          speed: settings.speed ?? 1,
          exaggeration: settings.exaggeration,
          cfg_weight: settings.cfg_weight,
          temperature: settings.temperature,
          top_p: settings.top_p,
          repetition_penalty: settings.repetition_penalty,
          seed: (settings.seed ?? seed) + itemIndex,
          output_format: settings.output_format ?? "wav",
          title: line.split(/[.!?]/)[0].slice(0, 56) || `Batch row ${itemIndex + 1}`,
          voice_name: settings.voice_name ?? "Default voice",
          priority: settings.priority ?? priority,
        });
        latest = next;
        onGenerated(next);
        setRecentOutputs((items) => [next, ...items.filter((item) => item.id !== next.id)].slice(0, 6));
        if (batchId) {
          const nextBatch = await updateBatchItem(batchId, itemIndex, "completed", next.id);
          setBatches((items) => [nextBatch, ...items.filter((item) => item.id !== nextBatch.id)]);
        }
      } catch (caught) {
        failed += 1;
        const message = caught instanceof Error ? caught.message : String(caught);
        if (batchId) {
          const nextBatch = await updateBatchItem(batchId, itemIndex, cancelRequested.current ? "cancelled" : "failed", undefined, message);
          setBatches((items) => [nextBatch, ...items.filter((item) => item.id !== nextBatch.id)]);
        }
        if (cancelRequested.current) throw new Error("Generation cancelled.");
        if (!batchId) throw caught;
      } finally {
        if (batchId) setBatchProgress({ completed: position + 1, total: rows.length, failed });
      }
    }
    return { latest, failed };
  }

  async function resumeBatch(batch: BatchRunRecord) {
    const rows = batch.items.filter((item) => item.status !== "completed").map((item) => ({ line: item.text, itemIndex: item.item_index }));
    if (!rows.length) return;
    setIsGenerating(true);
    cancelRequested.current = false;
    setError(undefined);
    setBatchProgress({ completed: 0, total: rows.length, failed: 0 });
    try {
      if (bootstrap.runtime === "tauri") {
        const updated = await resumeBatchRun(batch.id, parallelism, batch.status === "failed");
        setBatches((items) => [updated, ...items.filter((item) => item.id !== updated.id)]);
        return;
      }
      const outcome = await runScripts(rows, batch.request.settings ?? {}, batch.id);
      if (outcome.latest) setResult(outcome.latest);
      if (outcome.failed) setError(`${outcome.failed} recovered batch rows still failed.`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setIsGenerating(false);
    }
  }

  async function pauseBatch(batch: BatchRunRecord) {
    try {
      const updated = await pauseBatchRun(batch.id);
      setBatches((items) => [updated, ...items.filter((item) => item.id !== updated.id)]);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function cancelBatch(batch: BatchRunRecord) {
    try {
      const updated = await cancelBatchRun(batch.id);
      setBatches((items) => [updated, ...items.filter((item) => item.id !== updated.id)]);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function cancelGeneration() {
    cancelRequested.current = true;
    try {
      const activeBatches = batches.filter((batch) => ["queued", "running"].includes(batch.status));
      await Promise.all(activeBatches.map((batch) => cancelBatchRun(batch.id)));
      const activeJobs = jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status));
      await Promise.all(activeJobs.map((job) => cancelJob(job.id)));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function savePreset() {
    const name = window.prompt("Preset name", `${selectedModel?.engine ?? "Voice"} ${speed.toFixed(2)}x`);
    if (!name?.trim()) return;
    try {
      const saved = await saveGenerationPreset(name.trim(), {
        model_id: modelId,
        speaker: selectedVoice?.id ?? "default",
        language,
        speed,
        exaggeration,
        cfg_weight: cfgWeight,
        temperature,
        top_p: topP,
        repetition_penalty: repetitionPenalty,
        seed,
        output_format: outputFormat,
        input_mode: mode === "ssml" ? "ssml" : "text",
        reference_audio_path: selectedVoice?.state === "ready" ? selectedVoice.local_path : undefined,
        priority,
      });
      setPresets((items) => [saved, ...items.filter((item) => item.id !== saved.id)]);
      setSelectedPresetId(saved.id);
      setPresetState(`Saved preset: ${name.trim()}`);
    } catch (caught) {
      setPresetState(caught instanceof Error ? caught.message : String(caught));
    }
  }

  function applyPreset(id: string) {
    setSelectedPresetId(id);
    const preset = presets.find((item) => item.id === id);
    if (!preset) return;
    const settings = preset.settings;
    if (settings.model_id && ttsModels.some((model) => model.model_id === settings.model_id)) setModelId(settings.model_id);
    if (settings.speaker && voices.some((voice) => voice.id === settings.speaker)) setVoiceId(settings.speaker);
    if (settings.language) setLanguage(settings.language);
    if (typeof settings.speed === "number") setSpeed(settings.speed);
    if (typeof settings.exaggeration === "number") setExaggeration(settings.exaggeration);
    if (typeof settings.cfg_weight === "number") setCfgWeight(settings.cfg_weight);
    if (typeof settings.temperature === "number") setTemperature(settings.temperature);
    if (typeof settings.top_p === "number") setTopP(settings.top_p);
    if (typeof settings.repetition_penalty === "number") setRepetitionPenalty(settings.repetition_penalty);
    if (typeof settings.seed === "number") setSeed(settings.seed);
    if (settings.output_format) setOutputFormat(settings.output_format);
    if (settings.priority) setPriority(settings.priority);
    if (settings.reference_audio_path) {
      const matchingVoice = voices.find((voice) => voice.local_path === settings.reference_audio_path);
      if (matchingVoice) setVoiceId(matchingVoice.id);
    }
    setPresetState(`Applied preset: ${preset.name}`);
  }

  async function togglePlayback() {
    const audio = audioRef.current;
    if (!audio) return;
    if (!audio.paused) {
      audio.pause();
      setIsPlaying(false);
      return;
    }
    try {
      await audio.play();
      setPlaybackError(undefined);
      setIsPlaying(true);
    } catch (caught) {
      setPlaybackError(caught instanceof Error ? caught.message : "Audio playback failed");
    }
  }

  function seekPlayback(event: React.MouseEvent<HTMLButtonElement>) {
    const audio = audioRef.current;
    if (!audio || !Number.isFinite(audio.duration)) return;
    const bounds = event.currentTarget.getBoundingClientRect();
    const fraction = Math.min(1, Math.max(0, (event.clientX - bounds.left) / bounds.width));
    audio.currentTime = fraction * audio.duration;
    setPlaybackTime(audio.currentTime);
  }

  async function playRecentOutput(item: HistoryItem) {
    const audio = recentAudioRef.current;
    if (!audio || !item.audio_path) return;
    if (recentPlayingId === item.id && !audio.paused) {
      audio.pause();
      setRecentPlayingId(undefined);
      return;
    }
    try {
      audio.pause();
      audio.src = await loadGeneratedAudio(item.audio_path);
      await audio.play();
      setRecentPlayingId(item.id);
      setPlaybackError(undefined);
    } catch (caught) {
      setPlaybackError(caught instanceof Error ? caught.message : "Audio playback failed");
      setRecentPlayingId(undefined);
    }
  }

  async function deleteRecentOutput(item: HistoryItem) {
    if (!window.confirm(`Delete “${item.title}” and its generated audio?`)) return;
    try {
      if (await deleteHistoryItem(item.id, true)) {
        if (recentPlayingId === item.id) recentAudioRef.current?.pause();
        setRecentOutputs((items) => items.filter((candidate) => candidate.id !== item.id));
      }
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  const playbackProgress = playbackDuration > 0 ? playbackTime / playbackDuration : 0;
  const generationTypeControl = <Segmented
    label="Generation type"
    value={generationKind}
    onChange={setGenerationKind}
    options={[
      { value: "speech", label: "Voice" },
      { value: "music", label: "Music" },
    ]}
  />;
  const activeTaskCount = jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status)).length
    + batches.filter((batch) => ["queued", "running", "paused"].includes(batch.status)).length;
  const activeGenerationCount = jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status)).length;
  const generationCapacityReached = activeGenerationCount >= scheduler.max_workers;
  const musicInformationControl = <details className="music-info-disclosure music-header-info">
    <summary role="button" title="Music generation information" aria-label="Music generation information"><Info aria-hidden="true" size={14} /></summary>
    <div className="music-info-card" role="note">
      <strong>Local text-to-music</strong>
      <span>Direction + optional lyrics</span>
      <p>Direction and lyrics stay separate. The active model and duration determine the lyric limit; voice references, melody uploads, source audio, and batch music are outside this release.</p>
    </div>
  </details>;
  const generationToolbar = generationKind === "music" ? <>{musicInformationControl}{generationTypeControl}</> : generationTypeControl;
  const stagedJobs = jobs.filter((job) => activityStage === "queued"
    ? job.status === "queued"
    : activityStage === "progress"
      ? ["preparing", "running"].includes(job.status)
      : ["failed", "cancelled"].includes(job.status));
  const stagedBatches = batches.filter((batch) => activityStage === "queued"
    ? batch.status === "queued"
    : activityStage === "progress"
      ? ["running", "paused"].includes(batch.status)
      : ["completed", "failed", "cancelled"].includes(batch.status));
  const activityStageControl = <Segmented label="Activity stage" value={activityStage} onChange={setActivityStage} options={[
    { value: "queued", label: `Queued ${jobs.filter((job) => job.status === "queued").length + batches.filter((batch) => batch.status === "queued").length}` },
    { value: "progress", label: `In progress ${jobs.filter((job) => ["preparing", "running"].includes(job.status)).length + batches.filter((batch) => ["running", "paused"].includes(batch.status)).length}` },
    { value: "completed", label: `Completed ${recentOutputs.length + jobs.filter((job) => ["failed", "cancelled"].includes(job.status)).length}` },
  ]} />;

  function renderActivityRows() {
    const hasRows = stagedJobs.length > 0 || stagedBatches.length > 0 || (activityStage === "completed" && recentOutputs.length > 0);
    return <div className="queue-list staged-queue-list">
      {stagedJobs.slice(0, 6).map((job) => <div className="queue-row" key={job.id}><div><strong>{job.title?.slice(0, 32) || job.kind}</strong><StatusText tone={job.status === "failed" ? "danger" : job.status === "completed" ? "success" : job.status === "cancelled" ? "muted" : "warning"}>{job.status === "running" ? "Generating" : job.status}{job.priority && job.priority !== "normal" ? ` · ${job.priority}` : ""}</StatusText>{job.status === "failed" && jobFailureSummary(job.error) ? <small className="queue-error" title={job.error ?? undefined}>{jobFailureSummary(job.error)}</small> : null}</div><RowActionMenu label={`More options for ${job.title || job.kind}`} actions={[
        ...(["queued", "preparing", "running"].includes(job.status) ? [{ label: "Cancel generation", icon: <X size={12} />, danger: true, onSelect: async () => { await cancelJob(job.id); } }] : []),
        ...(["failed", "cancelled"].includes(job.status) && ["synthesis", "api-synthesis"].includes(job.kind) ? [{ label: "Retry generation", icon: <RotateCcw size={12} />, onSelect: () => retryQueuedJob(job) }] : []),
        ...(["completed", "failed", "cancelled"].includes(job.status) ? [{ label: "Clear finished task list", icon: <Trash2 size={12} />, danger: true, onSelect: clearFinishedQueue }] : []),
      ]} /></div>)}
      {stagedBatches.slice(0, 4).map((batch) => {
        const expanded = expandedBatchId === batch.id;
        const settling = batch.items.some((item) => item.status === "running") && batch.status === "paused";
        return <div className="batch-queue-entry" key={batch.id}><div className="queue-row batch-queue-row"><button className="batch-toggle" type="button" aria-expanded={expanded} onClick={() => setExpandedBatchId(expanded ? undefined : batch.id)}>{expanded ? <ChevronDown aria-hidden="true" size={13} /> : <ChevronRight aria-hidden="true" size={13} />}<span><strong>{batch.name}</strong><StatusText tone={batch.status === "completed" ? "success" : batch.status === "failed" ? "danger" : batch.status === "cancelled" ? "muted" : "warning"}>{batch.completed_items}/{batch.total_items} · {batch.status}{batch.priority !== "normal" ? ` · ${batch.priority}` : ""}</StatusText></span></button><RowActionMenu label={`More options for ${batch.name}`} actions={[
          { label: expanded ? "Hide batch rows" : "View batch rows", icon: expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />, onSelect: () => setExpandedBatchId(expanded ? undefined : batch.id) },
          ...(["queued", "running"].includes(batch.status) ? [{ label: "Pause batch", icon: <Pause size={12} />, onSelect: () => pauseBatch(batch) }, { label: "Cancel batch", icon: <X size={12} />, danger: true, onSelect: () => cancelBatch(batch) }] : []),
          ...(["paused", "failed"].includes(batch.status) ? [{ label: batch.status === "failed" ? "Retry failed rows" : "Resume batch", icon: <RotateCcw size={12} />, disabled: settling, onSelect: () => resumeBatch(batch) }, { label: "Cancel batch", icon: <X size={12} />, danger: true, onSelect: () => cancelBatch(batch) }] : []),
        ]} /></div>{expanded ? <div className="batch-item-list" aria-label={`${batch.name} rows`}>{batch.items.map((item) => <div className="batch-item-row" key={item.id}><span className="batch-item-index">{String(item.item_index + 1).padStart(2, "0")}</span><span className="batch-item-script" title={item.text}>{item.name || item.text}</span><StatusText tone={item.status === "completed" ? "success" : item.status === "failed" ? "danger" : item.status === "cancelled" ? "muted" : "warning"}>{item.status}</StatusText></div>)}</div> : null}</div>;
      })}
      {activityStage === "completed" ? recentOutputs.slice(0, 6).map((item) => <div className="queue-row recent-output-row" key={item.id}><button className="recent-output-play" type="button" disabled={!item.audio_path || item.missing} onClick={() => void playRecentOutput(item)} aria-label={`${recentPlayingId === item.id ? "Pause" : "Play"} ${item.title}`}>{recentPlayingId === item.id ? <Pause aria-hidden="true" fill="currentColor" size={11} /> : <Play aria-hidden="true" fill="currentColor" size={11} />}</button><div><strong>{item.title}</strong><StatusText tone="success">{item.generation_kind === "music" ? "Music" : item.voice} · {item.duration_seconds.toFixed(1)} s</StatusText></div><RowActionMenu label={`More options for ${item.title}`} actions={[
        { label: recentPlayingId === item.id ? "Pause audio" : "Play audio", icon: recentPlayingId === item.id ? <Pause size={12} /> : <Play size={12} />, disabled: !item.audio_path || item.missing, onSelect: () => playRecentOutput(item) },
        { label: "Open output folder", icon: <FolderOpen size={12} />, disabled: !item.audio_path || item.missing, onSelect: async () => { if (item.audio_path) await revealItemInDir(item.audio_path); } },
        { label: "Delete generated audio", icon: <Trash2 size={12} />, danger: true, onSelect: () => deleteRecentOutput(item) },
      ]} /></div>) : null}
      {!hasRows ? <div className="queue-empty-state"><strong>{activityStage === "queued" ? "Nothing queued" : activityStage === "progress" ? "No generation in progress" : "No completed audio"}</strong><span>{activityStage === "completed" ? "Finished speech and music appear here." : "New work will move here automatically."}</span></div> : null}
    </div>;
  }

  if (generationKind === "music") {
    return (
      <div className="page generate-page">
        <PageHeader title="New music generation" actions={generationToolbar} />
        <div className="music-generation-layout">
        <MusicGeneratePanel bootstrap={bootstrap} onGenerated={(item) => { onGenerated(item); setRecentOutputs((items) => [item, ...items.filter((existing) => existing.id !== item.id)].slice(0, 6)); }} />
        <div className="generation-activity-slot"><Panel className="runtime-rail generation-activity-drawer" ariaLabel="Generation activity">
          <div className="rail-heading">
            <div><span className="section-label">Activity</span><strong>Local generation queue</strong></div>
            <span className="rail-heading-actions"><StatusText tone={activeTaskCount ? "warning" : "muted"}>{activeTaskCount ? `${activeTaskCount} active` : "Ready"}</StatusText></span>
          </div>
          <div className="activity-stage-tabs">{activityStageControl}</div>
          {renderActivityRows()}
          <audio ref={recentAudioRef} className="visually-hidden" onEnded={() => setRecentPlayingId(undefined)} onPause={() => setRecentPlayingId(undefined)} />
          <div className="output-block"><strong>Local-only processing</strong><span>Music and speech outputs remain on this computer.</span></div>
        </Panel></div>
        </div>
      </div>
    );
  }

  return (
    <div className="page generate-page">
      <PageHeader title="New generation" actions={generationToolbar} />

      <div className="generate-layout">
        <Panel className="composer-panel" ariaLabel="Generation composer">
          <div className="composer-toolbar">
            <Segmented
              label="Composer mode"
              value={mode}
              onChange={setMode}
              options={[
                { value: "text", label: "Text" },
                { value: "ssml", label: "SSML" },
                { value: "batch", label: "Batch" },
              ]}
            />
            {presets.length ? <Dropdown ariaLabel="Generation preset" value={selectedPresetId} onChange={applyPreset} options={[{ value: "", label: "Preset" }, ...presets.map((preset) => ({ value: preset.id, label: preset.name }))]} /> : null}
            {mode === "batch" ? <button className="icon-button" type="button" title={bootstrap.runtime === "tauri" ? "Import TXT, CSV, or JSONL batch" : "Batch file import requires the desktop app"} disabled={bootstrap.runtime !== "tauri"} onClick={() => void importBatch()}><FileInput aria-hidden="true" size={14} /></button> : null}
            <span className="composer-count">{text.length} characters / about {estimatedSeconds} seconds</span>
          </div>

          <div className="script-box">
            <textarea
              aria-label="Script"
              value={text}
              onChange={(event) => { setText(event.target.value); setBatchImport(undefined); }}
              spellCheck="true"
              placeholder={mode === "batch" ? "Add one generation per line..." : mode === "ssml" ? "<speak>Write supported SSML here...</speak>" : "Write what the voice should say..."}
            />
            <span className="keyboard-hint">Ctrl + Enter to generate</span>
          </div>
          {mode === "batch" && batchImport ? <div className="batch-import-summary"><FileInput aria-hidden="true" size={13} /><strong>{batchImport.name}</strong><span>{batchImport.rows.length} rows / {batchImport.source_format.toUpperCase()}</span><button className="text-button" type="button" onClick={() => setBatchImport(undefined)}>Use edited lines</button></div> : null}

          <div className="generation-options">
          <div className="selector-grid">
            <SelectField label="Model" value={modelId} onChange={setModelId} status={selectedModel ? "Ready" : undefined} options={ttsModels.map((model) => ({ value: model.model_id, label: model.model_id }))} />
            <div className="voice-select-with-action">
              <SelectField label="Voice" value={voiceId} onChange={setVoiceId} status={selectedVoice ? `${selectedVoice.sample_seconds || "Preset"}` : undefined} options={compatibleVoices.map((voice) => ({ value: voice.id, label: `${voice.name} - ${voice.style}` }))} />
              <button className="icon-button" type="button" title="Add voice profile" aria-label="Add voice profile from Generate" onClick={() => setShowAddVoice(true)}>
                <Plus aria-hidden="true" size={14} />
              </button>
            </div>
          </div>

          <div className="route-control">
            <span className="field-label">Route</span>
            <Segmented label="Model route" value={routeIntent} onChange={applyRoute} options={[{ value: "manual", label: "Manual" }, { value: "fast", label: "Fast" }, { value: "expressive", label: "Expressive" }, { value: "clone", label: "Clone" }, { value: "multilingual", label: "Multilingual" }]} />
            {routeReason ? <StatusText tone={routeReason.startsWith("No installed") ? "warning" : "muted"}>{routeReason}</StatusText> : null}
          </div>

          <details className="generation-advanced">
            <summary><span><strong>Advanced settings</strong><small>Priority, pacing, sampling, language, and output</small></span><ChevronDown aria-hidden="true" size={14} /></summary>
            <div className="settings-grid">
            <CompactField label="Priority"><Dropdown ariaLabel="Queue priority" value={priority} onChange={(value) => setPriority(value as QueuePriority)} options={[{ value: "low", label: "Low" }, { value: "normal", label: "Normal" }, { value: "high", label: "High" }, { value: "urgent", label: "Urgent" }]} /></CompactField>
            {mode === "batch" ? <CompactField label="Parallel jobs">
              <div className="range-value">
                <input aria-label="Parallel jobs" min="1" max={bootstrap.scheduler.max_workers} step="1" type="range" value={parallelism} onChange={(event) => setParallelism(Number(event.target.value))} />
                <strong>{parallelism}/{bootstrap.scheduler.max_workers}</strong>
              </div>
            </CompactField> : null}
            {capability?.controls.speed ? <CompactField label="Speed">
              <div className="range-value">
                <input aria-label="Speed" min={capability.controls.speed.minimum} max={capability.controls.speed.maximum} step="0.05" type="range" value={speed} onChange={(event) => setSpeed(Number(event.target.value))} />
                <strong>{speed.toFixed(2)}x</strong>
              </div>
            </CompactField> : null}
            {capability?.controls.exaggeration ? <CompactField label="Exaggeration"><div className="range-value"><input aria-label="Exaggeration" min="0" max="1" step="0.05" type="range" value={exaggeration} onChange={(event) => setExaggeration(Number(event.target.value))} /><strong>{exaggeration.toFixed(2)}</strong></div></CompactField> : null}
            {capability?.controls.cfg_weight ? <CompactField label="CFG weight"><div className="range-value"><input aria-label="CFG weight" min="0" max="1" step="0.05" type="range" value={cfgWeight} onChange={(event) => setCfgWeight(Number(event.target.value))} /><strong>{cfgWeight.toFixed(2)}</strong></div></CompactField> : null}
            {capability?.controls.temperature ? <CompactField label="Temperature"><div className="range-value"><input aria-label="Temperature" min={capability.controls.temperature.minimum} max={capability.controls.temperature.maximum} step="0.05" type="range" value={temperature} onChange={(event) => setTemperature(Number(event.target.value))} /><strong>{temperature.toFixed(2)}</strong></div></CompactField> : null}
            {capability?.controls.top_p ? <CompactField label="Top P"><div className="range-value"><input aria-label="Top P" min={capability.controls.top_p.minimum} max={capability.controls.top_p.maximum} step="0.05" type="range" value={topP} onChange={(event) => setTopP(Number(event.target.value))} /><strong>{topP.toFixed(2)}</strong></div></CompactField> : null}
            {capability?.controls.repetition_penalty ? <CompactField label="Repetition"><div className="range-value"><input aria-label="Repetition penalty" min={capability.controls.repetition_penalty.minimum} max={capability.controls.repetition_penalty.maximum} step="0.05" type="range" value={repetitionPenalty} onChange={(event) => setRepetitionPenalty(Number(event.target.value))} /><strong>{repetitionPenalty.toFixed(2)}</strong></div></CompactField> : null}
            {capability && capability.languages.length > 1 ? <CompactField label="Language"><Dropdown ariaLabel="Language" value={language} onChange={setLanguage} options={capability.languages.map((value) => ({ value, label: value.toUpperCase() }))} /></CompactField> : null}
            <CompactField label="Seed">
              <input aria-label="Seed" type="number" value={seed} onChange={(event) => setSeed(Number(event.target.value))} />
            </CompactField>
            <CompactField label="Output">
              <Dropdown ariaLabel="Output format" value={outputFormat} onChange={(value) => setOutputFormat(value as "wav" | "flac")} options={[{ value: "wav", label: "WAV / source rate" }, { value: "flac", label: "FLAC / source rate" }]} />
            </CompactField>
            </div>
          </details>
          </div>

          {audioUrl || isAudioLoading || playbackError ? <div className="waveform-panel">
            <div className="waveform-meta">
              <span className="section-label">Preview</span>
              <span>{formatPlaybackTime(playbackTime)} / {formatPlaybackTime(playbackDuration)}</span>
            </div>
            <button className="audio-waveform" type="button" aria-label="Seek audio preview" disabled={!audioUrl} onClick={seekPlayback}>
              <span className="audio-track-progress" style={{ width: `${playbackProgress * 100}%` }} />
              <span className="audio-track-head" style={{ left: `${playbackProgress * 100}%` }} />
            </button>
            {audioUrl ? <audio ref={audioRef} className="visually-hidden" preload="auto" src={audioUrl} onCanPlay={() => setPlaybackError(undefined)} onLoadedMetadata={(event) => setPlaybackDuration(event.currentTarget.duration)} onTimeUpdate={(event) => setPlaybackTime(event.currentTarget.currentTime)} onError={() => setPlaybackError("Generated audio could not be decoded") } onEnded={(event) => { setIsPlaying(false); setPlaybackTime(event.currentTarget.duration); }} /> : null}
            <button className="icon-button preview-play" type="button" aria-label={isPlaying ? "Pause preview" : "Play preview"} title={isPlaying ? "Pause preview" : "Play preview"} disabled={!audioUrl || isAudioLoading} onClick={togglePlayback}>
              {isPlaying ? <Pause aria-hidden="true" fill="currentColor" size={13} /> : <Play aria-hidden="true" fill="currentColor" size={13} />}
            </button>
            {isAudioLoading ? <span className="playback-loading">Loading audio...</span> : null}
            {playbackError ? <span className="playback-error">{playbackError}</span> : null}
          </div> : null}

          <div className="composer-footer">
            <div className="composer-state">
              {error ? <StatusText tone="danger">{error}</StatusText> : null}
              {!error && isGenerating ? <StatusText tone="warning">{batchProgress ? `${batchProgress.completed}/${batchProgress.total} batch rows processed` : `${jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status)).length} task(s) active`}</StatusText> : null}
              {!error && !isGenerating ? (
                <StatusText tone={runtimeReady ? "success" : "warning"}>{runtimeReady ? (result ? `Ready / RTF ${result.rtf.toFixed(2)}x` : "Ready / first audio depends on engine") : "Runtime setup required"}</StatusText>
              ) : null}
            </div>
            {referenceRequired && !voiceReady ? <StatusText tone="warning">Add a clone-ready voice in Voices</StatusText> : null}
            <button className="button button-secondary" type="button" onClick={() => void savePreset()}>
              <Save aria-hidden="true" size={14} />
              Save preset
            </button>
            {isGenerating ? <button className="button button-secondary danger-button" type="button" onClick={() => void cancelGeneration()}><Pause aria-hidden="true" size={14} />Cancel all</button> : null}
            <button className="button button-primary" type="button" onClick={generate} disabled={!runtimeReady || !text.trim() || !modelId || !voiceReady || generationCapacityReached} title={generationCapacityReached ? `All ${scheduler.max_workers} generation slots are in use` : undefined}>
              {mode === "batch" ? "Start batch" : "Generate audio"}
            </button>
          </div>
          {presetState ? <div className="composer-message"><StatusText tone={presetState.startsWith("Saved") ? "success" : "danger"}>{presetState}</StatusText></div> : null}
        </Panel>

        <div className="generation-activity-slot"><Panel className="runtime-rail generation-activity-drawer" ariaLabel="Runtime and output queue">
          <div className="rail-heading">
            <div>
              <span className="section-label">Runtime</span>
              <strong>{selectedModel?.model_id.split("/").at(-1) ?? "No model"}</strong>
            </div>
            <span className="rail-heading-actions"><StatusText tone={runtimeReady && selectedModel ? "success" : "warning"}>{!runtimeReady ? "Setup required" : selectedModel ? `${activeGenerationCount}/${scheduler.max_workers} active` : "Install a model"}</StatusText></span>
          </div>

          <MetricStrip
            metrics={[
              { value: result ? `${result.inference_seconds.toFixed(2)} s` : "--", label: "Inference", tone: "success" },
              { value: result ? `${result.rtf.toFixed(2)}x` : "--", label: "RTF", tone: "success" },
              { value: result ? `${(result.vram_peak_mb / 1024).toFixed(1)} GB` : "--", label: "Peak VRAM", tone: "warning" },
            ]}
          />

          <div className="activity-stage-tabs">{activityStageControl}</div>
          {renderActivityRows()}
          <audio ref={recentAudioRef} className="visually-hidden" onEnded={() => setRecentPlayingId(undefined)} onPause={() => setRecentPlayingId(undefined)} />

          <span className="section-label rail-section">Output</span>
          <div className="output-block">
            <strong>{outputFormat.toUpperCase()} / source sample rate / mono</strong>
            <span>{bootstrap.export_dir}</span>
          </div>

          <div className="rail-actions">
            <button className="button button-secondary" type="button" disabled={!result?.audio_path} onClick={() => result?.audio_path && void revealItemInDir(result.audio_path)}>
              <FolderOpen aria-hidden="true" size={14} />
              Open output
            </button>
          </div>
        </Panel></div>
      </div>
      {showAddVoice ? <VoiceProfileDialog onClose={() => setShowAddVoice(false)} onCreated={useCreatedVoice} /> : null}
    </div>
  );
}
