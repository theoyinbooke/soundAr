import {
  ChevronDown,
  Clock3,
  FileAudio,
  FolderOpen,
  GripVertical,
  Info,
  ListMusic,
  Pause,
  Play,
  Plus,
  RotateCcw,
  Scissors,
  SlidersHorizontal,
  Star,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  cancelJob,
  generateMusic,
  getHistoryRequest,
  getSchedulerStatus,
  listHistory,
  listJobs,
  loadGeneratedAudio,
  pickMusicAudioFile,
  queueMusicGeneration,
  retryJob,
  updateHistoryMetadata,
} from "../lib/bridge";
import { capabilityForModel, qualifiedModels } from "../lib/capabilities";
import type {
  BootstrapState,
  CatalogModel,
  HistoryItem,
  JobRecord,
  MusicGenerationRequest,
  MusicLyricTiming,
  MusicSongSection,
  MusicStudioMode,
  QueuePriority,
} from "../types";
import { CompactField, Dropdown, MetricStrip, Panel, RowActionMenu, Segmented, SelectField, StatusText } from "../components/ui";

const DEFAULT_SECTIONS: MusicSongSection[] = [
  { id: "verse-1", type: "verse", label: "Verse 1", lyrics: "The city hums beneath the rain\nI trace your name across the windowpane" },
  { id: "chorus-1", type: "chorus", label: "Chorus", lyrics: "Hold the light until the morning comes\nWe are more than where we started from" },
  { id: "bridge-1", type: "bridge", label: "Bridge", lyrics: "Let the quiet turn to gold" },
  { id: "chorus-2", type: "chorus", label: "Final chorus", lyrics: "Hold the light until the morning comes" },
];

const SECTION_OPTIONS: Array<{ value: MusicSongSection["type"]; label: string }> = [
  { value: "intro", label: "Intro" },
  { value: "verse", label: "Verse" },
  { value: "pre-chorus", label: "Pre-chorus" },
  { value: "chorus", label: "Chorus" },
  { value: "bridge", label: "Bridge" },
  { value: "instrumental", label: "Instrumental" },
  { value: "outro", label: "Outro" },
];

function sectionLyrics(sections: MusicSongSection[]) {
  return sections
    .map((section) => `[${section.label}]\n${section.lyrics.trim()}`)
    .filter(Boolean)
    .join("\n\n");
}

function buildTiming(lyrics: string, duration: number): MusicLyricTiming[] {
  const lines = lyrics.split(/\r?\n/).map((line) => line.trim()).filter((line) => line && !/^\[.+\]$/.test(line));
  const slot = duration / Math.max(lines.length, 1);
  return lines.map((text, index) => ({
    id: `line-${index}`,
    text,
    start_seconds: Number((index * slot).toFixed(2)),
    end_seconds: Number(((index + 1) * slot).toFixed(2)),
  }));
}

function stageForJob(job: JobRecord) {
  if (job.stage) return job.stage;
  if (job.status === "queued") return "queued";
  if (job.status === "completed") return "completed";
  if (job.progress < 0.12) return "preparing";
  if (job.progress < 0.3) return "planning";
  if (job.progress < 0.78) return "rendering";
  if (job.progress < 0.94) return "decoding";
  return "finalizing";
}

function hardwareFit(model: CatalogModel, bootstrap: BootstrapState) {
  const installed = bootstrap.installed.some((item) => item.model_id === model.model_id);
  if (model.model_id === "ACE-Step/Ace-Step1.5") return {
    label: "Best fit",
    detail: "2B Turbo + local planner · CPU offload when needed",
    size: "~9 GB download",
    speed: "Near real time after warm-up",
    capabilities: "Songs · lyrics · references · extend · repaint",
    tone: "success" as const,
    installed,
  };
  if (model.model_id.includes("xl-turbo")) return {
    label: "Slower",
    detail: "XL quality tier · sustained CPU offload",
    size: "~10 GB download",
    speed: "Several minutes with 12 GB offload",
    capabilities: "Highest-quality song and instrumental renders",
    tone: "warning" as const,
    installed,
  };
  if (model.model_id.endsWith("v15-base")) return {
    label: "Tools",
    detail: "Base editor for multi-track operations",
    size: "~4.8 GB add-on",
    speed: "One local pass per requested stem",
    capabilities: "Vocals · drums · bass · other · cover · repaint",
    tone: "muted" as const,
    installed,
  };
  return {
    label: "Drafts",
    detail: "Short instrumental sketches",
    size: "~1.2 GB download",
    speed: "Faster short-form drafts",
    capabilities: "Instrumental text-to-music only",
    tone: "muted" as const,
    installed,
  };
}

function stemForResult(item: HistoryItem) {
  return item.title.match(/ · (vocals|drums|bass|other)$/i)?.[1];
}

function MusicResultCard({ item, onUse, onStems, onKeep }: { item: HistoryItem; onUse: (mode: "extend" | "edit-region" | "cover", item: HistoryItem) => void; onStems: (item: HistoryItem) => void; onKeep: (item: HistoryItem) => void }) {
  const [url, setUrl] = useState<string>();
  const [playing, setPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [resultSeed, setResultSeed] = useState<number>();
  const audioRef = useRef<HTMLAudioElement>(null);

  useEffect(() => {
    if (!item.audio_path) return;
    let active = true;
    let objectUrl: string | undefined;
    void loadGeneratedAudio(item.audio_path).then((next) => {
      objectUrl = next;
      if (active) setUrl(next);
    }).catch(() => undefined);
    return () => {
      active = false;
      if (objectUrl?.startsWith("blob:")) URL.revokeObjectURL(objectUrl);
    };
  }, [item.audio_path]);

  useEffect(() => {
    let active = true;
    void getHistoryRequest(item.id).then((request) => {
      if (active) setResultSeed(request.seed);
    }).catch(() => undefined);
    return () => { active = false; };
  }, [item.id]);

  async function toggle() {
    const audio = audioRef.current;
    if (!audio) return;
    if (!audio.paused) {
      audio.pause();
      setPlaying(false);
    } else {
      await audio.play();
      setPlaying(true);
    }
  }

  const stem = stemForResult(item);

  return <article className={`music-result-card${stem ? " is-stem" : ""}`}>
    <button className="music-result-play" type="button" disabled={!url} aria-label={`${playing ? "Pause" : "Play"} ${item.title}`} onClick={() => void toggle()}>
      {playing ? <Pause size={13} fill="currentColor" /> : <Play size={13} fill="currentColor" />}
    </button>
    <div className="music-result-main">
      <div className="music-result-title"><strong>{item.title}</strong><span>{stem ? `${stem} stem` : `${item.duration_seconds.toFixed(0)}s`}</span></div>
      <button className="music-mini-waveform" type="button" disabled={!url} aria-label={`Seek ${item.title}`} onClick={(event) => {
        const audio = audioRef.current;
        if (!audio || !Number.isFinite(audio.duration)) return;
        const bounds = event.currentTarget.getBoundingClientRect();
        audio.currentTime = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width)) * audio.duration;
      }}>
        {(item.waveform.length ? item.waveform : Array.from({ length: 48 }, (_, index) => 0.2 + Math.abs(Math.sin(index * 0.41)) * 0.7)).slice(0, 64).map((value, index) => <i key={index} style={{ height: `${Math.max(12, value * 100)}%` }} />)}
        <span style={{ width: `${item.duration_seconds ? (currentTime / item.duration_seconds) * 100 : 0}%` }} />
      </button>
      <div className="music-result-meta"><span>{item.model_id.split("/").at(-1)}</span><span>{resultSeed === undefined ? "Seed preserved" : `Seed ${resultSeed}`}</span><span>{item.rtf.toFixed(2)}× RTF</span></div>
    </div>
    <div className="music-result-actions">
      {!stem ? <button className="text-button" type="button" onClick={() => onUse("extend", item)}>Extend</button> : null}
      {!stem ? <button className="text-button" type="button" onClick={() => onUse("cover", item)}>Remix</button> : null}
      {!stem ? <button className="text-button" type="button" onClick={() => onUse("edit-region", item)}>Edit</button> : null}
      <button className="text-button" type="button" onClick={() => onKeep(item)}><Star size={11} fill={item.favorite ? "currentColor" : "none"} />{item.favorite ? "Kept" : "Keep"}</button>
      <RowActionMenu label={`More options for ${item.title}`} actions={[
        { label: "Separate stems", icon: <Scissors size={12} />, disabled: Boolean(stem) || !item.audio_path, onSelect: () => onStems(item) },
        { label: "Open output folder", icon: <FolderOpen size={12} />, disabled: !item.audio_path, onSelect: () => item.audio_path ? revealItemInDir(item.audio_path) : undefined },
      ]} />
    </div>
    {url ? <audio ref={audioRef} className="visually-hidden" src={url} onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)} onEnded={() => setPlaying(false)} /> : null}
  </article>;
}

export function MusicGeneratePanel({
  bootstrap,
  onGenerated,
  onOpenModels,
}: {
  bootstrap: BootstrapState;
  onGenerated: (item: HistoryItem) => void;
  onOpenModels?: () => void;
}) {
  const installedMusicModels = useMemo(() => qualifiedModels(bootstrap, "music"), [bootstrap]);
  const musicModels = useMemo(() => bootstrap.catalog.filter((model) => model.task === "music" && model.install_status !== "planned"), [bootstrap.catalog]);
  const preferredMusicModel = musicModels.find((model) => model.model_id === "ACE-Step/Ace-Step1.5")
    ?? musicModels.find((model) => capabilityForModel(bootstrap, model)?.music_features?.lyrics)
    ?? musicModels[0];
  const [modelId, setModelId] = useState(preferredMusicModel?.model_id ?? "");
  const [mode, setMode] = useState<MusicStudioMode>("song");
  const [quality, setQuality] = useState<"balanced" | "highest">("balanced");
  const [prompt, setPrompt] = useState("Warm, intimate indie-pop with brushed drums, soft electric piano, a restrained build, and a close-mic vocal performance.");
  const [sections, setSections] = useState<MusicSongSection[]>(DEFAULT_SECTIONS);
  const [duration, setDuration] = useState(90);
  const [variations, setVariations] = useState<1 | 2 | 4>(2);
  const [plannerEnabled, setPlannerEnabled] = useState(true);
  const [vocalLanguage, setVocalLanguage] = useState("en");
  const [inferenceSteps, setInferenceSteps] = useState(8);
  const [shift, setShift] = useState(3);
  const [bpm, setBpm] = useState(0);
  const [keyScale, setKeyScale] = useState("");
  const [timeSignature, setTimeSignature] = useState("4");
  const [seed, setSeed] = useState(42817);
  const [outputFormat, setOutputFormat] = useState<"wav" | "flac">("wav");
  const [priority, setPriority] = useState<QueuePriority>("normal");
  const [referenceAudio, setReferenceAudio] = useState("");
  const [sourceAudio, setSourceAudio] = useState("");
  const [referenceConsent, setReferenceConsent] = useState(false);
  const [consentBasis, setConsentBasis] = useState("");
  const [repaintStart, setRepaintStart] = useState(0);
  const [repaintEnd, setRepaintEnd] = useState(20);
  const [coverStrength, setCoverStrength] = useState(0.5);
  const [timing, setTiming] = useState<MusicLyricTiming[]>(() => buildTiming(sectionLyrics(DEFAULT_SECTIONS), 90));
  const [timingOpen, setTimingOpen] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [setupOpen, setSetupOpen] = useState(false);
  const [results, setResults] = useState<HistoryItem[]>([]);
  const [jobs, setJobs] = useState<JobRecord[]>(bootstrap.jobs.filter((job) => job.kind === "music-generation"));
  const [scheduler, setScheduler] = useState(bootstrap.scheduler);
  const [error, setError] = useState<string>();
  const submittedJobIds = useRef(new Set<string>());
  const deliveredHistoryIds = useRef(new Set<string>());

  const selectedModel = musicModels.find((model) => model.model_id === modelId);
  const capability = capabilityForModel(bootstrap, selectedModel);
  const features = capability?.music_features;
  const controls = capability?.controls;
  const lyrics = mode === "instrumental" ? "" : sectionLyrics(sections);
  const isAce = selectedModel?.engine === "acestep";
  const runtimeReady = bootstrap.runtime === "browser" || bootstrap.system.python_ready;
  const installedIds = new Set(bootstrap.installed.map((item) => item.model_id));
  const modelInstalled = bootstrap.runtime === "browser" || Boolean(selectedModel && installedIds.has(selectedModel.model_id));
  const sourceRequired = ["extend", "edit-region", "cover", "extract"].includes(mode);
  const referenceValid = !referenceAudio || (referenceConsent && consentBasis.trim().length > 0);
  const modelModeValid = isAce || ["song", "instrumental"].includes(mode);
  const modelLyricsValid = mode === "instrumental" || features?.lyrics === true || lyrics.trim().length === 0;
  const activeJobs = jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status));
  const availableSlots = Math.max(0, scheduler.max_workers - scheduler.active_workers);
  const canGenerate = runtimeReady && modelInstalled && Boolean(selectedModel) && modelModeValid && modelLyricsValid && prompt.trim().length > 0 && (!sourceRequired || sourceAudio) && referenceValid && variations <= availableSlots;

  useEffect(() => {
    if (!musicModels.some((model) => model.model_id === modelId)) setModelId(preferredMusicModel?.model_id ?? "");
  }, [modelId, musicModels, preferredMusicModel?.model_id]);

  useEffect(() => {
    setTiming(buildTiming(lyrics, duration));
  }, [lyrics, duration]);

  useEffect(() => {
    const minimum = controls?.duration_seconds?.minimum ?? 10;
    const maximum = controls?.duration_seconds?.maximum ?? 600;
    setDuration((value) => Math.min(maximum, Math.max(minimum, value)));
  }, [controls?.duration_seconds?.maximum, controls?.duration_seconds?.minimum]);

  useEffect(() => {
    if (quality === "balanced") {
      const studio = musicModels.find((model) => model.model_id === "ACE-Step/Ace-Step1.5");
      if (studio) setModelId(studio.model_id);
    } else {
      const xl = musicModels.find((model) => model.model_id.includes("xl-turbo"));
      if (xl) setModelId(xl.model_id);
    }
  }, [musicModels, quality]);

  useEffect(() => {
    let active = true;
    async function refresh() {
      try {
        const [nextJobs, nextHistory, nextScheduler] = await Promise.all([listJobs(), listHistory(), getSchedulerStatus()]);
        if (!active) return;
        const musicJobs = nextJobs.filter((job) => job.kind === "music-generation");
        setJobs(musicJobs);
        setScheduler(nextScheduler);
        const musicHistory = nextHistory.filter((item) => item.generation_kind === "music");
        setResults((current) => {
          if (bootstrap.runtime !== "browser") return musicHistory.slice(0, 12);
          const known = new Set(current.map((item) => item.id));
          return [...current, ...musicHistory.filter((item) => !known.has(item.id))].slice(0, 12);
        });
        for (const item of musicHistory) {
          if (item.job_id && submittedJobIds.current.has(item.job_id) && !deliveredHistoryIds.current.has(item.id)) {
            deliveredHistoryIds.current.add(item.id);
            onGenerated(item);
          }
        }
      } catch {
        // The next poll retries transient runtime startup or shutdown races.
      }
    }
    void refresh();
    const timer = window.setInterval(() => void refresh(), bootstrap.runtime === "tauri" ? 600 : 2_000);
    return () => { active = false; window.clearInterval(timer); };
  }, [bootstrap.runtime, onGenerated]);

  useEffect(() => {
    function shortcut(event: KeyboardEvent) {
      if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
        event.preventDefault();
        void generateVariations();
      }
    }
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [canGenerate, mode, modelId, outputFormat, prompt, sections, timing, variations]);

  function requestFor(index: number): MusicGenerationRequest {
    return {
      model_id: modelId,
      prompt: prompt.trim(),
      mode,
      quality_profile: quality,
      planner_enabled: plannerEnabled,
      variations,
      variation_index: index,
      lyrics: lyrics || undefined,
      song_sections: mode === "instrumental" ? undefined : sections,
      lyric_timing: mode === "instrumental" ? undefined : timing,
      vocal_language: mode === "instrumental" ? undefined : vocalLanguage,
      duration_seconds: duration,
      inference_steps: inferenceSteps,
      shift,
      bpm,
      key_scale: keyScale || undefined,
      time_signature: timeSignature || undefined,
      ...(referenceAudio ? {
        reference_audio_path: referenceAudio,
        reference_consent_confirmed: referenceConsent,
        reference_consent_basis: consentBasis.trim(),
      } : {}),
      ...(sourceAudio ? { source_audio_path: sourceAudio } : {}),
      repainting_start: mode === "edit-region" ? repaintStart : undefined,
      repainting_end: mode === "edit-region" ? repaintEnd : undefined,
      audio_cover_strength: ["cover", "edit-region"].includes(mode) ? coverStrength : undefined,
      return_lyric_timing: mode !== "instrumental",
      seed: seed + index,
      output_format: outputFormat,
      title: `${prompt.trim().split(/[.!?]/)[0].slice(0, 42)} · ${index + 1}`,
      priority,
    };
  }

  async function generateVariations() {
    if (!canGenerate) return;
    setError(undefined);
    try {
      if (bootstrap.runtime === "tauri") {
        const queued = await Promise.all(Array.from({ length: variations }, (_, index) => queueMusicGeneration(requestFor(index))));
        queued.forEach((job) => submittedJobIds.current.add(job.id));
        setJobs((items) => [...queued, ...items.filter((item) => !queued.some((queuedJob) => queuedJob.id === item.id))]);
      } else {
        const generated = await Promise.all(Array.from({ length: variations }, (_, index) => generateMusic(requestFor(index))));
        setResults((items) => [...generated, ...items].slice(0, 12));
        generated.forEach(onGenerated);
      }
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function chooseAudio(kind: "reference" | "source") {
    const path = await pickMusicAudioFile();
    if (!path) return;
    if (kind === "reference") {
      setReferenceAudio(path);
      setReferenceConsent(false);
      setConsentBasis("");
    } else {
      setSourceAudio(path);
    }
  }

  function addSection() {
    const count = sections.length + 1;
    setSections((items) => [...items, { id: crypto.randomUUID(), type: "verse", label: `Verse ${count}`, lyrics: "" }]);
  }

  function updateSection(id: string, changes: Partial<MusicSongSection>) {
    setSections((items) => items.map((item) => item.id === id ? { ...item, ...changes } : item));
  }

  function regenerateSection(section: MusicSongSection) {
    const source = results.find((item) => item.audio_path);
    if (!source?.audio_path) {
      setError("Render a first variation before regenerating one section.");
      return;
    }
    setMode("edit-region");
    setSourceAudio(source.audio_path);
    setPrompt(`${prompt} Regenerate only the ${section.label.toLowerCase()}: ${section.lyrics || "preserve the section role while creating a new musical idea"}.`);
    const sectionIndex = Math.max(0, sections.findIndex((item) => item.id === section.id));
    const sectionDuration = duration / Math.max(sections.length, 1);
    setRepaintStart(Number((sectionIndex * sectionDuration).toFixed(1)));
    setRepaintEnd(Number((Math.min(duration, (sectionIndex + 1) * sectionDuration)).toFixed(1)));
    window.scrollTo({ top: 0, behavior: "smooth" });
  }

  async function keepResult(item: HistoryItem) {
    try {
      const updated = await updateHistoryMetadata(item.id, { favorite: !item.favorite });
      setResults((items) => items.map((candidate) => candidate.id === item.id ? updated : candidate));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  function useResult(nextMode: "extend" | "edit-region" | "cover", item: HistoryItem) {
    if (!item.audio_path) {
      setError("This result has no local audio artifact to edit.");
      return;
    }
    setMode(nextMode);
    setSourceAudio(item.audio_path);
    setPrompt(nextMode === "extend" ? `${prompt} Continue naturally with a developed final section.` : prompt);
    window.scrollTo({ top: 0, behavior: "smooth" });
  }

  async function separateStems(item: HistoryItem) {
    const base = installedMusicModels.find((model) => model.model_id.endsWith("acestep-v15-base") && installedIds.has(model.model_id));
    if (!base || !item.audio_path) {
      setError("Install ACE-Step Base Tools from Models before separating vocals and instruments.");
      setSetupOpen(true);
      return;
    }
    setError(undefined);
    try {
      const stemTypes = ["vocals", "drums", "bass", "other"] as const;
      const queued = await Promise.all(stemTypes.map((stemType, index) => queueMusicGeneration({
        ...requestFor(index),
        model_id: base.model_id,
        mode: "extract",
        source_audio_path: item.audio_path ?? undefined,
        stem_type: stemType,
        variations: 1,
        title: `${item.title} · ${stemType}`,
        parent_history_id: item.id,
      })));
      queued.forEach((job) => submittedJobIds.current.add(job.id));
      setJobs((items) => [...queued, ...items]);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  const modeOptions = [
    { value: "song", label: "Song" },
    { value: "instrumental", label: "Instrumental" },
    { value: "extend", label: "Extend" },
    { value: "edit-region", label: "Edit region" },
  ];
  const activeStage = activeJobs[0] ? stageForJob(activeJobs[0]) : undefined;
  const renderSampleRate = features?.sample_rate ?? 48_000;
  const renderChannels = features?.channels ?? 2;
  const stemResults = results.filter((item) => stemForResult(item));
  const variationResults = results.filter((item) => !stemForResult(item));

  return <>
    <div className="music-studio-layout">
      <div className="music-studio-main">
        <Panel className="music-studio-composer" ariaLabel="Music studio composer">
          <div className="music-mode-row">
            <Segmented label="Music workflow" value={mode} onChange={(value) => setMode(value as MusicStudioMode)} options={modeOptions} />
            <button className="text-button" type="button" onClick={() => setSetupOpen(true)}><SlidersHorizontal size={12} />Studio setup</button>
          </div>

          <div className="music-direction-block">
            <label className="field-label" htmlFor="music-direction">Direction</label>
            <textarea id="music-direction" value={prompt} onChange={(event) => setPrompt(event.target.value.slice(0, 1_000))} placeholder="Describe the genre, instrumentation, mood, performance, and production…" />
            <span className="keyboard-hint">Ctrl + Enter · {prompt.length} / 1,000</span>
          </div>

          {mode !== "instrumental" ? <section className="song-structure" aria-labelledby="song-structure-title">
            <header><div><span className="section-label">Structure</span><strong id="song-structure-title">Song sections</strong></div><button className="button button-secondary" type="button" onClick={addSection}><Plus size={12} />Add section</button></header>
            <div className="song-section-list">
              {sections.map((section) => <article className="song-section-row" key={section.id}>
                <GripVertical aria-hidden="true" size={14} />
                <Dropdown ariaLabel={`${section.label} type`} value={section.type} onChange={(value) => updateSection(section.id, { type: value as MusicSongSection["type"], label: SECTION_OPTIONS.find((item) => item.value === value)?.label ?? section.label })} options={SECTION_OPTIONS} />
                <input aria-label={`${section.label} label`} value={section.label} onChange={(event) => updateSection(section.id, { label: event.target.value.slice(0, 40) })} />
                <textarea aria-label={`${section.label} lyrics`} value={section.lyrics} onChange={(event) => updateSection(section.id, { lyrics: event.target.value.slice(0, 1_200) })} placeholder={section.type === "instrumental" ? "Describe the instrumental passage…" : "Write the lines for this section…"} />
                <button className="text-button section-regenerate" type="button" title={`Regenerate ${section.label}`} onClick={() => regenerateSection(section)}><RotateCcw size={11} />Regenerate</button>
                <button className="icon-button" type="button" title={`Remove ${section.label}`} disabled={sections.length === 1} onClick={() => setSections((items) => items.filter((item) => item.id !== section.id))}><X size={13} /></button>
              </article>)}
            </div>
          </section> : null}

          {sourceRequired || isAce ? <section className="music-audio-conditioning">
            {sourceRequired ? <button className="audio-source-row" type="button" onClick={() => void chooseAudio("source")}><FileAudio size={15} /><span><strong>{sourceAudio ? sourceAudio.split("/").at(-1) : "Choose source audio"}</strong><small>Required for {mode === "edit-region" ? "region editing" : mode}</small></span><Upload size={13} /></button> : null}
            {isAce ? <button className="audio-source-row" type="button" onClick={() => void chooseAudio("reference")}><FileAudio size={15} /><span><strong>{referenceAudio ? referenceAudio.split("/").at(-1) : "Add style reference"}</strong><small>Optional · guides timbre and production</small></span><Upload size={13} /></button> : null}
            {referenceAudio ? <div className="reference-consent">
              <label><input type="checkbox" checked={referenceConsent} onChange={(event) => setReferenceConsent(event.target.checked)} />I own or have permission to use this audio.</label>
              <input aria-label="Reference audio permission basis" value={consentBasis} onChange={(event) => setConsentBasis(event.target.value.slice(0, 240))} placeholder="Permission basis, e.g. my original recording" />
            </div> : null}
            {mode === "edit-region" ? <div className="region-editor"><CompactField label="From"><input type="number" min="0" max={duration} value={repaintStart} onChange={(event) => setRepaintStart(Number(event.target.value))} /></CompactField><CompactField label="To"><input type="number" min={repaintStart + 1} max={duration} value={repaintEnd} onChange={(event) => setRepaintEnd(Number(event.target.value))} /></CompactField><CompactField label="Preserve source"><div className="range-value"><input aria-label="Source preservation" type="range" min="0" max="1" step="0.05" value={coverStrength} onChange={(event) => setCoverStrength(Number(event.target.value))} /><strong>{Math.round(coverStrength * 100)}%</strong></div></CompactField></div> : null}
          </section> : null}

          <div className="music-primary-settings">
            <SelectField label="Music model" value={modelId} onChange={setModelId} status={modelInstalled ? "Ready" : "Setup"} options={musicModels.map((model) => ({ value: model.model_id, label: model.model_id }))} />
            <CompactField label="Quality"><Segmented label="Quality" value={quality} onChange={(value) => setQuality(value as "balanced" | "highest")} options={[{ value: "balanced", label: "Balanced" }, { value: "highest", label: "Highest" }]} /></CompactField>
            <CompactField label="Variations"><Segmented label="Variations" value={String(variations)} onChange={(value) => setVariations(Number(value) as 1 | 2 | 4)} options={[{ value: "1", label: "1" }, { value: "2", label: "2" }, { value: "4", label: "4" }]} /></CompactField>
          </div>

          <details className="music-studio-disclosure" open={timingOpen} onToggle={(event) => setTimingOpen(event.currentTarget.open)}>
            <summary><span><Clock3 size={13} />Lyric timing</span><small>{timing.length} lines · editable LRC timing</small><ChevronDown size={13} /></summary>
            <div className="lyric-timing-list">{timing.map((line, index) => <div className="lyric-timing-row" key={line.id}><span>{String(index + 1).padStart(2, "0")}</span><input aria-label={`Lyric line ${index + 1}`} value={line.text} onChange={(event) => setTiming((items) => items.map((item) => item.id === line.id ? { ...item, text: event.target.value } : item))} /><input aria-label={`Lyric line ${index + 1} start`} type="number" min="0" step="0.1" value={line.start_seconds} onChange={(event) => setTiming((items) => items.map((item) => item.id === line.id ? { ...item, start_seconds: Number(event.target.value) } : item))} /><span>→</span><input aria-label={`Lyric line ${index + 1} end`} type="number" min="0.1" step="0.1" value={line.end_seconds} onChange={(event) => setTiming((items) => items.map((item) => item.id === line.id ? { ...item, end_seconds: Number(event.target.value) } : item))} /></div>)}</div>
          </details>

          <details className="music-studio-disclosure" open={advancedOpen} onToggle={(event) => setAdvancedOpen(event.currentTarget.open)}>
            <summary><span><SlidersHorizontal size={13} />Generation settings</span><small>{duration}s · {bpm ? `${bpm} BPM` : "Auto tempo"} · seed {seed}</small><ChevronDown size={13} /></summary>
            <div className="music-advanced-grid">
              <CompactField label="Duration"><div className="range-value"><input aria-label="Music duration" type="range" min={controls?.duration_seconds?.minimum ?? 10} max={Math.min(controls?.duration_seconds?.maximum ?? 600, 600)} step="5" value={duration} onChange={(event) => setDuration(Number(event.target.value))} /><strong>{duration}s</strong></div></CompactField>
              <CompactField label="Tempo"><input aria-label="Music tempo" type="number" min="0" max="300" value={bpm} onChange={(event) => setBpm(Number(event.target.value))} placeholder="Auto" /></CompactField>
              <CompactField label="Key"><input aria-label="Music key" value={keyScale} onChange={(event) => setKeyScale(event.target.value.slice(0, 24))} placeholder="Auto" /></CompactField>
              <CompactField label="Meter"><Dropdown ariaLabel="Time signature" value={timeSignature} onChange={setTimeSignature} options={[{ value: "4", label: "4/4" }, { value: "3", label: "3/4" }, { value: "6", label: "6/8" }, { value: "2", label: "2/4" }]} /></CompactField>
              <CompactField label="Vocal language"><Dropdown ariaLabel="Vocal language" value={vocalLanguage} onChange={setVocalLanguage} options={(capability?.languages ?? ["en"]).map((language) => ({ value: language, label: language.toUpperCase() }))} /></CompactField>
              <CompactField label="Refinement"><div className="range-value"><input aria-label="Music refinement steps" type="range" min={1} max={quality === "highest" ? 20 : 12} step="1" value={inferenceSteps} onChange={(event) => setInferenceSteps(Number(event.target.value))} /><strong>{inferenceSteps}</strong></div></CompactField>
              <CompactField label="Prompt shift"><div className="range-value"><input aria-label="Music prompt shift" type="range" min="1" max="5" step="0.1" value={shift} onChange={(event) => setShift(Number(event.target.value))} /><strong>{shift.toFixed(1)}</strong></div></CompactField>
              <CompactField label="Seed"><input aria-label="Music seed" type="number" min="0" max="4294967295" value={seed} onChange={(event) => setSeed(Number(event.target.value))} /></CompactField>
              <CompactField label="Priority"><Dropdown ariaLabel="Music queue priority" value={priority} onChange={(value) => setPriority(value as QueuePriority)} options={[{ value: "low", label: "Low" }, { value: "normal", label: "Normal" }, { value: "high", label: "High" }, { value: "urgent", label: "Urgent" }]} /></CompactField>
              <CompactField label="Output"><Dropdown ariaLabel="Music output format" value={outputFormat} onChange={(value) => setOutputFormat(value as "wav" | "flac")} options={[{ value: "wav", label: "WAV" }, { value: "flac", label: "FLAC" }]} /></CompactField>
              <label className="music-planner-toggle"><input type="checkbox" checked={plannerEnabled} onChange={(event) => setPlannerEnabled(event.target.checked)} /><span><strong>Local song planner</strong><small>Plan structure, metadata, lyric language, and semantic audio codes.</small></span></label>
            </div>
          </details>

          <footer className="music-composer-footer">
            <div>{error ? <StatusText tone="danger">{error}</StatusText> : !modelInstalled ? <StatusText tone="warning">Set up the selected model before generating.</StatusText> : !modelModeValid ? <StatusText tone="warning">This workflow needs ACE-Step Studio or Base Tools.</StatusText> : !modelLyricsValid ? <StatusText tone="warning">MusicGen is instrumental-only. Choose ACE-Step or switch to Instrumental.</StatusText> : sourceRequired && !sourceAudio ? <StatusText tone="warning">Choose source audio to continue.</StatusText> : !referenceValid ? <StatusText tone="warning">Record permission for the reference audio.</StatusText> : variations > availableSlots ? <StatusText tone="warning">{availableSlots} local slot{availableSlots === 1 ? "" : "s"} available.</StatusText> : <StatusText tone="success">Ready · local only · {renderChannels === 2 ? "stereo" : "mono"} {(renderSampleRate / 1_000).toFixed(0)} kHz</StatusText>}</div>
            <button className="button button-primary" type="button" disabled={!canGenerate} onClick={() => void generateVariations()}>{bootstrap.runtime === "tauri" ? `Generate ${variations === 1 ? "song" : `${variations} variations`}` : `Preview ${variations === 1 ? "song" : `${variations} variations`}`}</button>
          </footer>
        </Panel>

        <section className="music-results-section" aria-labelledby="music-results-title">
          <header><div><span className="section-label">Results</span><strong id="music-results-title">Variations</strong></div><span>{results.length ? `${Math.min(results.length, 12)} recent` : "Nothing rendered yet"}</span></header>
          <div className="music-results-list">{variationResults.length ? variationResults.map((item) => <MusicResultCard key={item.id} item={item} onUse={useResult} onKeep={(selected) => void keepResult(selected)} onStems={(selected) => void separateStems(selected)} />) : <div className="music-results-empty"><ListMusic size={18} /><strong>Your finished variations will appear here</strong><span>Listen, compare, extend, edit, keep, or separate a result without leaving Generate.</span></div>}</div>
          {stemResults.length ? <section className="music-stems-section" aria-labelledby="music-stems-title"><header><div><span className="section-label">Multitrack</span><strong id="music-stems-title">Separated stems</strong></div><span>{stemResults.length} tracks</span></header><div className="music-results-list">{stemResults.map((item) => <MusicResultCard key={item.id} item={item} onUse={useResult} onKeep={(selected) => void keepResult(selected)} onStems={(selected) => void separateStems(selected)} />)}</div></section> : null}
        </section>
      </div>

      <Panel className="music-runtime-rail" ariaLabel="Music runtime">
        <div className="rail-heading"><div><span className="section-label">Runtime</span><strong>{selectedModel?.model_id.split("/").at(-1) ?? "No model"}</strong></div><button className="icon-button" type="button" title="Music model setup" onClick={() => setSetupOpen(true)}><Info size={13} /></button></div>
        <MetricStrip metrics={[
          { value: `${scheduler.active_workers}/${scheduler.max_workers}`, label: "Local slots", tone: activeJobs.length ? "warning" : "success" },
          { value: activeStage ? activeStage.replace("-", " ") : "Ready", label: "Current stage", tone: activeStage ? "warning" : "success" },
          { value: `${(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB`, label: "GPU memory" },
        ]} />
        <div className="music-stage-track" aria-label="Generation stages">{["Prepare", "Plan", "Render", "Decode", "Finish"].map((stage, index) => {
          const order = ["preparing", "planning", "rendering", "decoding", "finalizing"];
          const activeIndex = activeStage ? order.indexOf(activeStage) : -1;
          return <div className={activeIndex > index ? "is-complete" : activeIndex === index ? "is-active" : ""} key={stage}><i /><span>{stage}</span></div>;
        })}</div>
        <span className="section-label rail-section">Work</span>
        <div className="queue-list">{activeJobs.length ? activeJobs.slice(0, 6).map((job) => <div className="queue-row music-job-row" key={job.id}><div><strong>{job.title?.slice(0, 32) || "Music variation"}</strong><StatusText tone="warning">{stageForJob(job)} · {Math.round(job.progress * 100)}% · ~{Math.max(1, Math.ceil(duration * (1 - job.progress) * (quality === "highest" ? 1.4 : 0.7)))}s</StatusText></div><RowActionMenu label={`More options for ${job.title ?? "music task"}`} actions={[{ label: "Cancel generation", icon: <X size={12} />, danger: true, onSelect: async () => { await cancelJob(job.id); } }]} /></div>) : <div className="queue-empty-state"><strong>No generation in progress</strong><span>Up to {scheduler.max_workers} variations can run concurrently.</span></div>}</div>
        {jobs.filter((job) => ["failed", "cancelled"].includes(job.status)).slice(0, 2).map((job) => <button className="music-retry-row" type="button" key={job.id} onClick={async () => { const next = await retryJob(job.id); submittedJobIds.current.add(job.id); setJobs((items) => items.map((item) => item.id === job.id ? next : item)); }}><RotateCcw size={12} /><span><strong>{job.title ?? "Music task"}</strong><small>{job.status} · retry</small></span></button>)}
        <div className="music-runtime-summary"><div><span>Profile</span><strong>{quality === "balanced" ? "2B Turbo + planner" : "XL Turbo + offload"}</strong></div><div><span>Output</span><strong>{outputFormat.toUpperCase()} · {(renderSampleRate / 1_000).toFixed(0)} kHz · {renderChannels === 2 ? "stereo" : "mono"}</strong></div><div><span>License</span><strong>{selectedModel?.license ?? "Model-specific"}</strong></div></div>
        <button className="button button-secondary" type="button" onClick={() => void revealItemInDir(bootstrap.export_dir)}><FolderOpen size={13} />Open output folder</button>
      </Panel>
    </div>

    {setupOpen ? <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setSetupOpen(false); }}><section className="modal music-setup-modal" role="dialog" aria-modal="true" aria-labelledby="music-setup-title">
      <header className="modal-header"><div><h2 id="music-setup-title">Music studio setup</h2><p>Choose the model path that fits this Linux workstation.</p></div><button className="icon-button" type="button" title="Close" onClick={() => setSetupOpen(false)}><X size={14} /></button></header>
      <div className="modal-body">
        <div className="music-hardware-row"><span><strong>{bootstrap.system.gpu_name || "Local GPU"}</strong><small>{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB VRAM · {bootstrap.system.cuda_available ? "CUDA ready" : "CPU mode"}</small></span><StatusText tone={bootstrap.system.vram_total_mb >= 11_000 ? "success" : "warning"}>{bootstrap.system.vram_total_mb >= 11_000 ? "Studio capable" : "Offload required"}</StatusText></div>
        <div className="music-model-options">{musicModels.map((model) => { const fit = hardwareFit(model, bootstrap); return <button className={modelId === model.model_id ? "is-selected" : ""} type="button" key={model.model_id} onClick={() => { setModelId(model.model_id); setQuality(model.model_id.includes("xl-turbo") ? "highest" : "balanced"); }}><span className="music-model-radio" /><span className="music-model-copy"><strong>{model.model_id.split("/").at(-1)}</strong><small>{fit.detail}</small><em>{model.summary}</em><span className="music-model-facts"><small>{fit.size}</small><small>{fit.speed}</small><small>{model.license ?? "Model-specific license"} · {model.access ?? "public"}</small><small>{fit.capabilities}</small></span></span><span><StatusText tone={fit.tone}>{fit.label}</StatusText><small>{fit.installed || bootstrap.runtime === "browser" ? "Installed" : "Not installed"}</small></span></button>; })}</div>
        <div className="music-setup-note"><Info size={14} /><span>Balanced is the automatic 12 GB default. Highest quality keeps XL opt-in because CPU offload makes it slower. Base Tools is selected only for stems and multi-track edits.</span></div>
      </div>
      <footer className="modal-actions"><button className="button button-secondary" type="button" onClick={() => setSetupOpen(false)}>Done</button>{onOpenModels ? <button className="button button-primary" type="button" onClick={() => { setSetupOpen(false); onOpenModels(); }}>Manage model files</button> : null}</footer>
    </section></div> : null}
  </>;
}
