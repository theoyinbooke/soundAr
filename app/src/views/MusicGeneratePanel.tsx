import { FolderOpen, Pause, Play, RotateCcw, Sparkles } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { cancelJob, generateMusic, getSchedulerStatus, listHistory, listJobs, loadGeneratedAudio, queueMusicGeneration, retryJob } from "../lib/bridge";
import { capabilityForModel, qualifiedModels } from "../lib/capabilities";
import type { BootstrapState, HistoryItem, JobRecord, MusicGenerationRequest, QueuePriority } from "../types";
import { CompactField, Dropdown, MetricStrip, Panel, SelectField, StatusText } from "../components/ui";

const idleWaveform = Array.from({ length: 64 }, (_, index) => 0.12 + Math.abs(Math.sin(index * 0.31)) * 0.13);

function formatPlaybackTime(seconds: number) {
  if (!Number.isFinite(seconds)) return "0:00.0";
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds % 60).toFixed(1).padStart(4, "0")}`;
}

export function MusicGeneratePanel({
  bootstrap,
  onGenerated,
}: {
  bootstrap: BootstrapState;
  onGenerated: (item: HistoryItem) => void;
}) {
  const musicModels = useMemo(() => qualifiedModels(bootstrap, "music"), [bootstrap]);
  const preferredMusicModel = musicModels.find((model) => capabilityForModel(bootstrap, model)?.music_features?.lyrics) ?? musicModels[0];
  const [modelId, setModelId] = useState(preferredMusicModel?.model_id ?? "");
  const [prompt, setPrompt] = useState("Warm, intimate indie-pop with brushed drums, soft electric piano, a restrained build, and a close-mic vocal performance.");
  const [lyrics, setLyrics] = useState("[Verse]\nThe city hums beneath the rain\nI trace your name across the windowpane\n\n[Chorus]\nHold the light until the morning comes\nWe are more than where we started from");
  const [vocalLanguage, setVocalLanguage] = useState("en");
  const [duration, setDuration] = useState(20);
  const [guidanceScale, setGuidanceScale] = useState(3);
  const [temperature, setTemperature] = useState(1);
  const [topK, setTopK] = useState(250);
  const [topP, setTopP] = useState(0);
  const [inferenceSteps, setInferenceSteps] = useState(8);
  const [shift, setShift] = useState(3);
  const [bpm, setBpm] = useState(0);
  const [seed, setSeed] = useState(42817);
  const [outputFormat, setOutputFormat] = useState<"wav" | "flac">("wav");
  const [priority, setPriority] = useState<QueuePriority>("normal");
  const [result, setResult] = useState<HistoryItem>();
  const [jobs, setJobs] = useState<JobRecord[]>(bootstrap.jobs.filter((job) => job.kind === "music-generation"));
  const [scheduler, setScheduler] = useState(bootstrap.scheduler);
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string>();
  const [audioUrl, setAudioUrl] = useState<string>();
  const [playbackError, setPlaybackError] = useState<string>();
  const [isAudioLoading, setIsAudioLoading] = useState(false);
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackTime, setPlaybackTime] = useState(0);
  const [playbackDuration, setPlaybackDuration] = useState(0);
  const audioRef = useRef<HTMLAudioElement>(null);
  const submittedJobIds = useRef(new Set<string>());
  const deliveredHistoryIds = useRef(new Set<string>());
  const selectedModel = musicModels.find((model) => model.model_id === modelId);
  const capability = capabilityForModel(bootstrap, selectedModel);
  const controls = capability?.controls;
  const runtimeReady = bootstrap.runtime === "browser" || bootstrap.system.python_ready;
  const durationControl = controls?.duration_seconds;
  const guidanceControl = controls?.guidance_scale;
  const temperatureControl = controls?.temperature;
  const topKControl = controls?.top_k;
  const topPControl = controls?.top_p;
  const inferenceStepsControl = controls?.inference_steps;
  const shiftControl = controls?.shift;
  const bpmControl = controls?.bpm;
  const musicFeatures = capability?.music_features;
  const supportsLyrics = musicFeatures?.lyrics === true;
  const lyricLimit = musicFeatures?.max_lyrics_characters ?? 1_200;
  const lyricDurationLimit = musicFeatures?.max_lyrics_characters_per_second
    ? Math.max(160, Math.floor(duration * musicFeatures.max_lyrics_characters_per_second))
    : lyricLimit;
  const usableLyricLimit = Math.min(lyricLimit, lyricDurationLimit);
  const lyricsValidForModel = (!lyrics.trim() || supportsLyrics) && lyrics.length <= usableLyricLimit;
  const renderChannels = musicFeatures?.channels ?? 1;
  const renderSampleRate = musicFeatures?.sample_rate ?? 32_000;
  const vramEnvelopeMb = capability?.minimum_vram_mb ?? 0;
  const validSeed = Number.isInteger(seed) && seed >= 0 && seed <= 4_294_967_295;

  useEffect(() => {
    if (!musicModels.some((model) => model.model_id === modelId)) {
      setModelId(preferredMusicModel?.model_id ?? "");
    }
  }, [modelId, musicModels, preferredMusicModel?.model_id]);

  useEffect(() => {
    const supportedLanguages = capability?.languages ?? [];
    if (supportsLyrics && supportedLanguages.length && !supportedLanguages.includes(vocalLanguage)) {
      setVocalLanguage(supportedLanguages[0]);
    }
  }, [capability?.languages, supportsLyrics, vocalLanguage]);

  useEffect(() => {
    function clamp(value: number, control: typeof durationControl | undefined) {
      if (!control) return value;
      return Math.min(control.maximum, Math.max(control.minimum, value));
    }
    setDuration((value) => clamp(value, durationControl));
    setGuidanceScale((value) => clamp(value, guidanceControl));
    setTemperature((value) => clamp(value, temperatureControl));
    setTopK((value) => clamp(value, topKControl));
    setTopP((value) => clamp(value, topPControl));
    setInferenceSteps((value) => clamp(value, inferenceStepsControl));
    setShift((value) => clamp(value, shiftControl));
    setBpm((value) => clamp(value, bpmControl));
  }, [bpmControl, durationControl, guidanceControl, inferenceStepsControl, shiftControl, temperatureControl, topKControl, topPControl]);

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
        const [nextJobs, nextHistory, nextScheduler] = await Promise.all([
          listJobs(),
          listHistory(),
          getSchedulerStatus(),
        ]);
        if (!active) return;
        const musicJobs = nextJobs.filter((job) => job.kind === "music-generation");
        setJobs(musicJobs);
        setScheduler(nextScheduler);
        setIsGenerating(musicJobs.some((job) => submittedJobIds.current.has(job.id) && ["queued", "preparing", "running"].includes(job.status)));
        for (const item of nextHistory) {
          if (
            item.generation_kind === "music"
            && item.job_id
            && submittedJobIds.current.has(item.job_id)
            && !deliveredHistoryIds.current.has(item.id)
          ) {
            deliveredHistoryIds.current.add(item.id);
            setResult(item);
            onGenerated(item);
          }
        }
      } catch {
        // Poll again after transient runtime startup or shutdown races.
      }
    }
    void refreshQueue();
    const timer = window.setInterval(() => void refreshQueue(), 500);
    return () => { active = false; window.clearInterval(timer); };
  }, [bootstrap.runtime, onGenerated]);

  useEffect(() => {
    function handleShortcut(event: KeyboardEvent) {
      if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
        event.preventDefault();
        void generate();
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "p" && audioUrl) {
        event.preventDefault();
        void togglePlayback();
      }
    }
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, [audioUrl, bpm, duration, guidanceScale, inferenceSteps, lyrics, modelId, outputFormat, priority, prompt, seed, shift, temperature, topK, topP, vocalLanguage]);

  function currentRequest(): MusicGenerationRequest {
    const request: MusicGenerationRequest = {
      model_id: modelId,
      prompt: prompt.trim(),
      duration_seconds: duration,
      seed,
      output_format: outputFormat,
      title: prompt.trim().slice(0, 56),
      priority,
    };
    if (lyrics.trim()) {
      request.lyrics = lyrics.trim();
      request.vocal_language = vocalLanguage;
    }
    if (guidanceControl) request.guidance_scale = guidanceScale;
    if (temperatureControl) request.temperature = temperature;
    if (topKControl) request.top_k = topK;
    if (topPControl) request.top_p = topP;
    if (inferenceStepsControl) request.inference_steps = inferenceSteps;
    if (shiftControl) request.shift = shift;
    if (bpmControl) request.bpm = bpm;
    return request;
  }

  async function generate() {
    if (!runtimeReady || !selectedModel || !prompt.trim() || !lyricsValidForModel || !validSeed) return;
    setError(undefined);
    try {
      const request = currentRequest();
      if (bootstrap.runtime === "tauri") {
        const queued = await queueMusicGeneration(request);
        submittedJobIds.current.add(queued.id);
        setJobs((items) => [queued, ...items.filter((item) => item.id !== queued.id)]);
        setIsGenerating(true);
        return;
      }
      setIsGenerating(true);
      const generated = await generateMusic(request);
      setResult(generated);
      onGenerated(generated);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      if (bootstrap.runtime !== "tauri") setIsGenerating(false);
    }
  }

  async function cancelGeneration(job: JobRecord) {
    try {
      await cancelJob(job.id);
      setJobs((items) => items.map((item) => item.id === job.id ? { ...item, status: "cancelled" } : item));
      setIsGenerating(false);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function retryMusicJob(job: JobRecord) {
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

  const waveform = result?.waveform?.length ? result.waveform : idleWaveform;
  const playbackProgress = playbackDuration > 0 ? playbackTime / playbackDuration : 0;
  const activeJobs = jobs.filter((job) => ["queued", "preparing", "running"].includes(job.status));
  const retryableJobs = jobs.filter((job) => ["failed", "cancelled"].includes(job.status));

  return (
    <div className="generate-layout">
      <Panel className="composer-panel" ariaLabel="Music generation composer">
        <div className="composer-toolbar">
          <span className="section-label">Text-to-music / local only</span>
          <span className="composer-count">Direction {prompt.length} / 1,000 · Lyrics {lyrics.length} / {usableLyricLimit}</span>
        </div>

        <div className="music-input-grid">
          <div className="script-box">
            <label className="field-label" htmlFor="music-direction">Music direction <em>Required</em></label>
            <textarea
              id="music-direction"
              aria-label="Music direction"
              value={prompt}
              onChange={(event) => setPrompt(event.target.value.slice(0, 1_000))}
              spellCheck="true"
              placeholder="Describe genre, instruments, mood, tempo, arrangement, and vocal character..."
            />
            <span className="keyboard-hint">Ctrl + Enter to generate</span>
          </div>
          <div className="script-box">
            <label className="field-label" htmlFor="music-lyrics">Lyrics or text to sing <em>Optional</em></label>
            <textarea
              id="music-lyrics"
              aria-label="Lyrics or text to sing"
              value={lyrics}
              onChange={(event) => setLyrics(event.target.value.slice(0, lyricLimit))}
              spellCheck="true"
              placeholder={'[Verse]\nWrite the words the vocalist should sing…\n\n[Chorus]\nUse section labels for a clearer arrangement.'}
            />
            <span className="keyboard-hint">[Verse] · [Chorus] · {lyrics.length} / {usableLyricLimit}</span>
          </div>
        </div>
        {!supportsLyrics && lyrics.trim() ? <StatusText tone="warning">{selectedModel?.model_id.split("/").at(-1) ?? "This model"} is instrumental-only. Choose ACE-Step or clear the lyric field.</StatusText> : null}
        {lyrics.length > usableLyricLimit ? <StatusText tone="warning">The lyric text is too long for this model and duration. Shorten it to {usableLyricLimit} characters or increase duration.</StatusText> : null}
        <StatusText tone="muted">Direction and lyrics stay separate. Voice references, melody uploads, source audio, and batch generation are intentionally outside this release.</StatusText>

        <div className="selector-grid">
          <SelectField
            label="Music model"
            value={modelId}
            onChange={setModelId}
            status={selectedModel ? "Ready" : undefined}
            disabled={!musicModels.length}
            options={musicModels.map((model) => ({ value: model.model_id, label: model.model_id }))}
          />
          <div className="field">
            <span className="field-label">Render profile</span>
            <div className="output-block"><strong>{renderChannels === 1 ? "Mono" : "Stereo"} / {(renderSampleRate / 1_000).toFixed(0)} kHz</strong><span>{supportsLyrics ? "Lyric-conditioned local render" : "Instrumental local draft"}</span></div>
          </div>
        </div>

        <span className="section-label">Generation settings</span>
        <div className="settings-grid">
          <CompactField label="Priority"><Dropdown ariaLabel="Music queue priority" value={priority} onChange={(value) => setPriority(value as QueuePriority)} options={[{ value: "low", label: "Low" }, { value: "normal", label: "Normal" }, { value: "high", label: "High" }, { value: "urgent", label: "Urgent" }]} /></CompactField>
          <CompactField label="Duration">
            <div className="range-value"><input aria-label="Music duration" type="range" min={durationControl?.minimum ?? 4} max={durationControl?.maximum ?? 30} step="1" value={duration} onChange={(event) => setDuration(Number(event.target.value))} /><strong>{duration}s</strong></div>
          </CompactField>
          {supportsLyrics && lyrics.trim() ? <CompactField label="Vocal language"><Dropdown ariaLabel="Vocal language" value={vocalLanguage} onChange={setVocalLanguage} options={(capability?.languages ?? ["en"]).map((language) => ({ value: language, label: language.toUpperCase() }))} /></CompactField> : null}
          {guidanceControl ? <CompactField label="Guidance">
            <div className="range-value"><input aria-label="Guidance scale" type="range" min={guidanceControl?.minimum ?? 1} max={guidanceControl?.maximum ?? 6} step="0.1" value={guidanceScale} onChange={(event) => setGuidanceScale(Number(event.target.value))} /><strong>{guidanceScale.toFixed(1)}</strong></div>
          </CompactField> : null}
          {temperatureControl ? <CompactField label="Temperature">
            <div className="range-value"><input aria-label="Music temperature" type="range" min={temperatureControl?.minimum ?? 0.1} max={temperatureControl?.maximum ?? 2} step="0.05" value={temperature} onChange={(event) => setTemperature(Number(event.target.value))} /><strong>{temperature.toFixed(2)}</strong></div>
          </CompactField> : null}
          {topKControl ? <CompactField label="Top K">
            <div className="range-value"><input aria-label="Music top k" type="range" min={topKControl?.minimum ?? 0} max={topKControl?.maximum ?? 250} step="1" value={topK} onChange={(event) => setTopK(Number(event.target.value))} /><strong>{topK}</strong></div>
          </CompactField> : null}
          {topPControl ? <CompactField label="Top P">
            <div className="range-value"><input aria-label="Music top p" type="range" min={topPControl?.minimum ?? 0} max={topPControl?.maximum ?? 1} step="0.05" value={topP} onChange={(event) => setTopP(Number(event.target.value))} /><strong>{topP.toFixed(2)}</strong></div>
          </CompactField> : null}
          {inferenceStepsControl ? <CompactField label="Refinement">
            <div className="range-value"><input aria-label="Music refinement steps" type="range" min={inferenceStepsControl.minimum} max={inferenceStepsControl.maximum} step="1" value={inferenceSteps} onChange={(event) => setInferenceSteps(Number(event.target.value))} /><strong>{inferenceSteps}</strong></div>
          </CompactField> : null}
          {shiftControl ? <CompactField label="Prompt adherence">
            <div className="range-value"><input aria-label="Music prompt adherence" type="range" min={shiftControl.minimum} max={shiftControl.maximum} step="0.1" value={shift} onChange={(event) => setShift(Number(event.target.value))} /><strong>{shift.toFixed(1)}</strong></div>
          </CompactField> : null}
          {bpmControl ? <CompactField label="Tempo">
            <div className="range-value"><input aria-label="Music tempo" type="range" min={bpmControl.minimum} max={bpmControl.maximum} step="1" value={bpm} onChange={(event) => setBpm(Number(event.target.value))} /><strong>{bpm ? `${bpm} BPM` : "Auto"}</strong></div>
          </CompactField> : null}
          <CompactField label="Seed"><input aria-label="Music seed" type="number" min="0" max="4294967295" step="1" value={seed} onChange={(event) => setSeed(Number(event.target.value))} /></CompactField>
          <CompactField label="Output"><Dropdown ariaLabel="Music output format" value={outputFormat} onChange={(value) => setOutputFormat(value as "wav" | "flac")} options={[{ value: "wav", label: `WAV / ${(renderSampleRate / 1_000).toFixed(0)} kHz` }, { value: "flac", label: `FLAC / ${(renderSampleRate / 1_000).toFixed(0)} kHz` }]} /></CompactField>
        </div>

        <div className="waveform-panel">
          <div className="waveform-meta"><span className="section-label">Preview</span><span>{formatPlaybackTime(playbackTime)} / {formatPlaybackTime(playbackDuration)}</span></div>
          <button className="audio-waveform" type="button" aria-label="Seek music preview" disabled={!audioUrl} onClick={seekPlayback}>
            {waveform.map((peak, index) => <i className={(index + 1) / waveform.length <= playbackProgress ? "is-played" : ""} key={`${index}-${peak}`} style={{ height: `${Math.max(4, Math.round(peak * 32))}px` }} />)}
          </button>
          {audioUrl ? <audio ref={audioRef} className="visually-hidden" preload="auto" src={audioUrl} onCanPlay={() => setPlaybackError(undefined)} onLoadedMetadata={(event) => setPlaybackDuration(event.currentTarget.duration)} onTimeUpdate={(event) => setPlaybackTime(event.currentTarget.currentTime)} onError={() => setPlaybackError("Generated music could not be decoded")} onEnded={(event) => { setIsPlaying(false); setPlaybackTime(event.currentTarget.duration); }} /> : null}
          <button className="icon-button preview-play" type="button" title={isPlaying ? "Pause preview" : "Play preview"} disabled={!audioUrl || isAudioLoading} onClick={() => void togglePlayback()}>{isPlaying ? <Pause aria-hidden="true" fill="currentColor" size={13} /> : <Play aria-hidden="true" fill="currentColor" size={13} />}</button>
          {isAudioLoading ? <span className="playback-loading">Loading audio...</span> : null}
          {playbackError ? <span className="playback-error">{playbackError}</span> : null}
        </div>

        <div className="composer-footer">
          <div className="composer-state">
            {error ? <StatusText tone="danger">{error}</StatusText> : null}
            {!error && isGenerating ? <StatusText tone="warning">Music generation is queued locally.</StatusText> : null}
            {!error && !isGenerating ? <StatusText tone={!musicModels.length || !runtimeReady || !lyricsValidForModel ? "warning" : "success"}>{!musicModels.length ? "Install a local music model from Models to generate locally" : !lyricsValidForModel ? "ACE-Step is required to render the entered lyrics." : !runtimeReady ? "Runtime setup required" : result?.preview ? "Browser preview has no rendered audio" : result ? `Ready / RTF ${result.rtf.toFixed(2)}x` : "Ready / first render depends on GPU"}</StatusText> : null}
          </div>
          <button className="button button-primary" type="button" onClick={() => void generate()} disabled={!runtimeReady || !selectedModel || !prompt.trim() || !lyricsValidForModel || !validSeed || isGenerating}>
            <Sparkles aria-hidden="true" size={14} />
            {bootstrap.runtime === "tauri" ? "Queue music" : "Preview music flow"}
          </button>
        </div>
      </Panel>

      <Panel className="runtime-rail" ariaLabel="Music runtime and output queue">
        <div className="rail-heading">
          <div><span className="section-label">Runtime</span><strong>{selectedModel?.model_id.split("/").at(-1) ?? "No music model"}</strong></div>
          <StatusText tone={runtimeReady && selectedModel ? "success" : "warning"}>{!runtimeReady ? "Setup required" : selectedModel ? `${scheduler.active_workers}/${scheduler.max_workers} active` : "Install a model"}</StatusText>
        </div>
        <MetricStrip metrics={[
          { value: result ? `${result.inference_seconds.toFixed(2)} s` : "--", label: "Inference", tone: "success" },
          { value: result ? `${result.rtf.toFixed(2)}x` : "--", label: "RTF", tone: "success" },
          { value: result ? `${(result.vram_peak_mb / 1024).toFixed(1)} GB` : vramEnvelopeMb ? `${(vramEnvelopeMb / 1024).toFixed(1)} GB` : "--", label: "VRAM envelope", tone: "warning" },
        ]} />

        <span className="section-label rail-section">Queue</span>
        <div className="queue-list">
          {activeJobs.slice(0, 5).map((job) => <div className="queue-row" key={job.id}><div><strong>{job.title?.slice(0, 28) || "Music draft"}</strong><StatusText tone="warning">{job.status}{job.priority && job.priority !== "normal" ? ` / ${job.priority}` : ""}</StatusText></div><button className="icon-button" type="button" title="Cancel music task" onClick={() => void cancelGeneration(job)}><Pause aria-hidden="true" size={13} /></button></div>)}
          {retryableJobs.slice(0, 3).map((job) => <div className="queue-row" key={job.id}><div><strong>{job.title?.slice(0, 28) || "Music draft"}</strong><StatusText tone={job.status === "failed" ? "danger" : "muted"}>{job.status}{job.attempt > 1 ? ` / attempt ${job.attempt}` : ""}</StatusText></div><button className="text-button queue-resume" type="button" onClick={() => void retryMusicJob(job)}><RotateCcw aria-hidden="true" size={12} />Retry</button></div>)}
          {!activeJobs.length && !retryableJobs.length ? <div className="queue-row"><div><strong>{prompt.slice(0, 28) || "Music prompt"}</strong><StatusText tone={result ? "success" : "muted"}>{result ? "Ready" : "Draft"}</StatusText></div></div> : null}
        </div>

        <span className="section-label rail-section">Output</span>
        <div className="output-block"><strong>{outputFormat.toUpperCase()} / {(renderSampleRate / 1_000).toFixed(0)} kHz / {renderChannels === 1 ? "mono" : "stereo"}</strong><span>{bootstrap.export_dir}</span></div>
        <dl className="compact-definition-list"><div><dt>Scope</dt><dd>Direction + optional lyrics</dd></div><div><dt>License</dt><dd>{selectedModel?.license ?? "Model-specific"}</dd></div><div><dt>Release cap</dt><dd>{durationControl ? `${durationControl.minimum}–${durationControl.maximum} seconds` : "Bounded"}</dd></div></dl>
        <div className="rail-actions"><button className="button button-secondary" type="button" disabled={!result?.audio_path} onClick={() => result?.audio_path && void revealItemInDir(result.audio_path)}><FolderOpen aria-hidden="true" size={14} />Open output</button></div>
      </Panel>
    </div>
  );
}
