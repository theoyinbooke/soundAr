import { Activity, Check, CircleStop, Copy, Download, Eye, FolderOpen, Gauge, KeyRound, LoaderCircle, Mic, NotebookPen, Pause, Play, RefreshCw, Search, Server, SlidersHorizontal, Star, Trophy, Trash2, Volume2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { ApplicationSettings, AudioInputDevice, AudioOutputDevice, AudioPlaybackState, AudioRecordingState, BootstrapState, ComparisonRecord, DeveloperApiState, HistoryFilters, HistoryItem } from "../types";
import { CompactAudioPlayer, CompactField, EmptyState, MetricStrip, PageHeader, Panel, RowActionMenu, Segmented, SelectField, StatusText } from "../components/ui";
import { BrandLockup, BrandMark } from "../components/Brand";
import { cancelComparison, createComparison, deleteHistoryItem, duplicateHistoryItem, exportHistoryItem, getAudioPlaybackStatus, getAudioRecordingStatus, getComparison, getDeveloperApiStatus, getHistoryRequest, listAudioInputDevices, listAudioOutputDevices, listHistory, loadGeneratedAudio, loadTranscriptionAudio, startAudioPlayback, startAudioRecording, startDeveloperApi, stopAudioPlayback, stopAudioRecording, stopDeveloperApi, synthesizeSpeech, transcribeAudio, updateComparisonReview, updateHistoryMetadata } from "../lib/bridge";
import { canSynthesizeWithoutReference, qualifiedModels } from "../lib/capabilities";

const idleWaveform = [22, 34, 18, 48, 28, 62, 42, 71, 38, 54, 31, 66, 45, 57];

export function reconcileAudioDeviceSelection<T extends { id: string; is_default: boolean }>(current: string, devices: T[]) {
  if (devices.some((device) => device.id === current)) return current;
  return devices.find((device) => device.is_default)?.id ?? devices[0]?.id ?? "";
}

export function LiveView({ bootstrap }: { bootstrap: BootstrapState }) {
  const sttModels = qualifiedModels(bootstrap, "stt");
  const streamingModels = qualifiedModels(bootstrap, "tts").filter((model) => bootstrap.engine_capabilities.find((engine) => engine.id === model.engine)?.streaming);
  const [devices, setDevices] = useState<AudioInputDevice[]>([]);
  const [outputs, setOutputs] = useState<AudioOutputDevice[]>([]);
  const [deviceId, setDeviceId] = useState("");
  const [outputId, setOutputId] = useState("");
  const [recording, setRecording] = useState<AudioRecordingState>({ recording: false });
  const [playback, setPlayback] = useState<AudioPlaybackState>({ playing: false });
  const [audioUrl, setAudioUrl] = useState<string>();
  const [transcript, setTranscript] = useState("");
  const [vadEnabled, setVadEnabled] = useState(true);
  const [autoStop, setAutoStop] = useState(false);
  const [silenceMs, setSilenceMs] = useState(1200);
  const [inputGain, setInputGain] = useState(1);
  const [speechCleanup, setSpeechCleanup] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [devicesChanged, setDevicesChanged] = useState(false);
  const inputIds = useRef("");
  const outputIds = useRef("");

  async function refreshDevices(reportChange = false) {
    const [available, availableOutputs] = await Promise.all([listAudioInputDevices(), listAudioOutputDevices()]);
    const nextInputIds = available.map((device) => device.id).sort().join("\n");
    const nextOutputIds = availableOutputs.map((device) => device.id).sort().join("\n");
    if (reportChange && (nextInputIds !== inputIds.current || nextOutputIds !== outputIds.current)) setDevicesChanged(true);
    inputIds.current = nextInputIds;
    outputIds.current = nextOutputIds;
    setDevices(available);
    setOutputs(availableOutputs);
    setDeviceId((current) => reconcileAudioDeviceSelection(current, available));
    setOutputId((current) => reconcileAudioDeviceSelection(current, availableOutputs));
  }

  useEffect(() => {
    void Promise.all([refreshDevices(), getAudioRecordingStatus(), getAudioPlaybackStatus()]).then(([, status, playbackStatus]) => {
      setRecording(status);
      setPlayback(playbackStatus);
    }).catch((caught) => setError(caught instanceof Error ? caught.message : String(caught)));
  }, []);
  useEffect(() => {
    const timer = window.setInterval(() => void refreshDevices(true).catch((caught) => setError(caught instanceof Error ? caught.message : String(caught))), 2_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (!devicesChanged) return;
    const timer = window.setTimeout(() => setDevicesChanged(false), 3_000);
    return () => window.clearTimeout(timer);
  }, [devicesChanged]);
  useEffect(() => {
    if (!recording.recording) return;
    const timer = window.setInterval(() => void getAudioRecordingStatus().then(async (status) => {
      setRecording(status);
      if (!status.recording && status.audio_path) {
        if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
        setAudioUrl(await loadTranscriptionAudio(status.audio_path));
      }
    }).catch((caught) => {
      setRecording({ recording: false });
      setError(caught instanceof Error ? caught.message : String(caught));
    }), 80);
    return () => window.clearInterval(timer);
  }, [recording.recording, audioUrl]);
  useEffect(() => {
    if (!playback.playing) return;
    const timer = window.setInterval(() => void getAudioPlaybackStatus().then(setPlayback).catch((caught) => setError(caught instanceof Error ? caught.message : String(caught))), 80);
    return () => window.clearInterval(timer);
  }, [playback.playing]);
  useEffect(() => () => { if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl); }, [audioUrl]);

  async function toggleRecording() {
    setError(undefined);
    try {
      if (recording.recording) {
        const result = await stopAudioRecording(); setRecording(result);
        if (result.audio_path) {
          if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
          setAudioUrl(await loadTranscriptionAudio(result.audio_path));
        }
      } else {
        setTranscript("");
        if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
        setAudioUrl(undefined);
        setRecording(await startAudioRecording({ device_id: deviceId || undefined, vad_enabled: vadEnabled, auto_stop: autoStop, silence_ms: silenceMs, input_gain: inputGain }));
      }
    } catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  async function transcribeCapture() {
    if (!recording.audio_path || !sttModels[0] || busy) return;
    setBusy(true); setError(undefined);
    try { setTranscript((await transcribeAudio(sttModels[0].model_id, recording.audio_path, speechCleanup)).text); }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setBusy(false); }
  }

  async function toggleRoutedPlayback() {
    if (!recording.audio_path) return;
    setError(undefined);
    try {
      setPlayback(playback.playing ? await stopAudioPlayback() : await startAudioPlayback(recording.audio_path, outputId || undefined));
    } catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  const peak = Math.max(0, Math.min(1, recording.peak ?? 0));
  const levels = Array.from({ length: 32 }, (_, index) => Math.max(5, Math.round(peak * 72 * (0.45 + Math.abs(Math.sin(index * 1.71)) * 0.55))));

  return (
    <div className="page">
      <PageHeader title="Live" subtitle="Capture local microphone audio, monitor input, review it, and transcribe without a cloud service." actions={<button className={`button ${recording.recording ? "button-secondary danger-button" : "button-primary"}`} type="button" disabled={!devices.length} onClick={() => void toggleRecording()}>{recording.recording ? <CircleStop size={14} /> : <Mic size={14} />}{recording.recording ? "Stop capture" : "Record"}</button>} />
      <MetricStrip metrics={[{ value: `${(recording.duration_seconds ?? 0).toFixed(1)} s`, label: "Capture" }, { value: `${(recording.speech_seconds ?? 0).toFixed(1)} s`, label: "Detected speech" }, { value: `${Math.round(peak * 100)}%`, label: "Input peak" }, { value: String(recording.dropped_frames ?? 0), label: "Dropped frames" }]} />
      <div className="live-layout">
        <Panel className="live-stage" ariaLabel="Live voice session">
          <div className={`live-orb ${recording.recording ? "is-active" : ""}`}><Activity size={22} /></div>
          <div><h2>{recording.recording ? recording.speech_active ? "Voice detected" : recording.speech_detected ? "Listening for more" : "Listening" : recording.audio_path ? "Capture ready" : "Microphone ready"}</h2><p>{recording.recording ? recording.device_name : recording.audio_path ? `${(recording.duration_seconds ?? 0).toFixed(1)} seconds captured as a managed WAV${recording.stop_reason === "silence" ? " after silence was detected" : ""}.` : "Select an input, then start a visible local recording."}</p></div>
          <div className="level-bars" aria-hidden="true">{levels.map((height, index) => <i key={`${height}-${index}`} style={{ height }} />)}</div>
          <CompactAudioPlayer src={audioUrl} label="captured audio" />
          <StatusText tone={error || recording.capture_error ? "danger" : recording.recording && recording.speech_active ? "success" : "muted"}>{error ?? recording.capture_error ?? (recording.recording ? `${vadEnabled ? "Adaptive voice detection active" : "Voice detection off"} / ${(recording.buffered_frames ?? 0)} frames buffered` : recording.stop_reason === "silence" ? "Auto-stopped after trailing silence" : "Microphone access is off")}</StatusText>
          {transcript ? <p className="live-transcript">{transcript}</p> : null}
        </Panel>
        <Panel className="live-controls" ariaLabel="Live session settings">
          <span className="section-label">Capture</span>
          <SelectField label="Audio input" value={deviceId} onChange={setDeviceId} disabled={recording.recording || !devices.length} options={devices.map((device) => ({ value: device.id, label: `${device.name}${device.is_default ? " (default)" : ""}` }))} />
          <label className="live-range"><span><strong>Input gain</strong><small>{inputGain.toFixed(2)}x</small></span><input aria-label="Input gain" type="range" min="0.25" max="4" step="0.05" value={inputGain} disabled={recording.recording} onChange={(event) => setInputGain(Number(event.target.value))} /></label>
          <label className="toggle-row live-toggle"><span><strong>Voice detection</strong><small>Adaptive local energy gate</small></span><input aria-label="Voice detection" type="checkbox" checked={vadEnabled} disabled={recording.recording} onChange={(event) => { setVadEnabled(event.target.checked); if (!event.target.checked) setAutoStop(false); }} /></label>
          <label className="toggle-row live-toggle"><span><strong>Stop after silence</strong><small>Only after speech begins</small></span><input aria-label="Stop after silence" type="checkbox" checked={autoStop} disabled={recording.recording || !vadEnabled} onChange={(event) => setAutoStop(event.target.checked)} /></label>
          <SelectField label="Trailing silence" value={String(silenceMs)} onChange={(value) => setSilenceMs(Number(value))} disabled={recording.recording || !autoStop} options={[{ value: "800", label: "0.8 seconds" }, { value: "1200", label: "1.2 seconds" }, { value: "2000", label: "2.0 seconds" }, { value: "3000", label: "3.0 seconds" }]} />
          <div className="live-output-routing">
            <SelectField label="Audio output" value={outputId} onChange={setOutputId} disabled={recording.recording || playback.playing || !outputs.length} options={outputs.map((device) => ({ value: device.id, label: `${device.name}${device.is_default ? " (default)" : ""}` }))} />
            <button className="button button-secondary live-action" type="button" disabled={!recording.audio_path || !outputs.length} onClick={() => void toggleRoutedPlayback()}>{playback.playing ? <CircleStop size={13} /> : <Volume2 size={13} />}{playback.playing ? "Stop output" : "Play on output"}</button>
            {playback.audio_path ? <div className="routed-progress" aria-label="Routed playback progress"><i style={{ width: `${Math.round((playback.progress ?? 0) * 100)}%` }} /></div> : null}
          </div>
          {playback.playback_error ? <StatusText tone="danger">{playback.playback_error}</StatusText> : null}
          <button className="button button-secondary live-action" type="button" disabled={!recording.audio_path || recording.recording || !sttModels.length || busy} onClick={() => void transcribeCapture()}>{busy ? <LoaderCircle className="spin" size={13} /> : <NotebookPen size={13} />}{busy ? "Transcribing" : "Transcribe capture"}</button>
          <label className="toggle-row live-toggle"><span><strong>Speech cleanup</strong><small>Process a derived copy for Whisper</small></span><input aria-label="Speech cleanup" type="checkbox" checked={speechCleanup} disabled={busy || recording.recording} onChange={(event) => setSpeechCleanup(event.target.checked)} /></label>
          <dl className="compact-definition-list control-summary"><div><dt>Input</dt><dd>{devices.length ? recording.sample_rate ? `${(recording.sample_rate / 1000).toFixed(1)} kHz` : "Native" : "No input"}</dd></div><div><dt>Output</dt><dd>{playback.playing ? `${playback.output_sample_rate ? (playback.output_sample_rate / 1000).toFixed(1) : "--"} kHz` : outputs.length ? "Native" : "No output"}</dd></div><div><dt>Underruns</dt><dd>{playback.underrun_frames ?? 0}</dd></div><div><dt>Cloud access</dt><dd>Off</dd></div></dl>
          {devicesChanged ? <StatusText tone="success">Audio devices refreshed</StatusText> : null}
          {!streamingModels.length ? <StatusText tone="warning">Voice transformation stays locked until a streaming engine passes local latency and soak tests.</StatusText> : null}
        </Panel>
      </div>
    </div>
  );
}

export function CompareView({ bootstrap, onGenerated }: { bootstrap: BootstrapState; onGenerated?: (item: HistoryItem) => void }) {
  const models = qualifiedModels(bootstrap, "tts").filter((model) => canSynthesizeWithoutReference(bootstrap, model));
  const initialModels = Array.from({ length: 4 }, (_, index) => models[index % Math.max(1, models.length)]?.model_id ?? "");
  const [takeCount, setTakeCount] = useState(2);
  const [takeModels, setTakeModels] = useState(initialModels);
  const [script, setScript] = useState("Every voice has a texture. The right model preserves clarity without flattening the intent behind the words.");
  const [runs, setRuns] = useState(bootstrap.comparisons);
  const [comparison, setComparison] = useState<ComparisonRecord>();
  const [urls, setUrls] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [playingId, setPlayingId] = useState<string>();
  const [progress, setProgress] = useState(0);
  const audioRefs = useRef<Record<string, HTMLAudioElement | null>>({});
  const urlsRef = useRef<Record<string, string>>({});
  const pollingRef = useRef(false);
  const [error, setError] = useState<string>();
  const running = busy || comparison?.status === "queued" || comparison?.status === "running";

  useEffect(() => { urlsRef.current = urls; }, [urls]);
  useEffect(() => () => Object.values(urlsRef.current).forEach((url) => { if (url.startsWith("blob:")) URL.revokeObjectURL(url); }), []);
  useEffect(() => {
    if (!comparison || bootstrap.runtime !== "tauri" || !running) return;
    const timer = window.setInterval(() => {
      if (pollingRef.current) return;
      pollingRef.current = true;
      void getComparison(comparison.id)
        .then(applyRun)
        .catch((caught) => setError(caught instanceof Error ? caught.message : String(caught)))
        .finally(() => { pollingRef.current = false; });
    }, 350);
    return () => window.clearInterval(timer);
  }, [bootstrap.runtime, comparison?.id, running]);

  async function applyRun(run: ComparisonRecord) {
    const nextUrls: Record<string, string> = {};
    for (const take of run.takes) {
      if (take.result?.audio_path) {
        nextUrls[take.id] = urlsRef.current[take.id] ?? await loadGeneratedAudio(take.result.audio_path);
        onGenerated?.(take.result);
      }
    }
    const stale = Object.entries(urlsRef.current).filter(([id]) => !(id in nextUrls));
    stale.forEach(([, url]) => { if (url.startsWith("blob:")) URL.revokeObjectURL(url); });
    urlsRef.current = nextUrls;
    setUrls(nextUrls);
    setComparison(run);
    setRuns((current) => [run, ...current.filter((item) => item.id !== run.id)]);
  }

  async function renderTakes() {
    if (running || !script.trim() || takeModels.slice(0, takeCount).some((model) => !model)) return;
    setBusy(true);
    setError(undefined);
    try {
      setProgress(0);
      const created = await createComparison({ script: script.trim(), blind: true, priority: "high", takes: takeModels.slice(0, takeCount).map((modelId, index) => {
        const model = models.find((entry) => entry.model_id === modelId);
        return {
          model_id: modelId,
          speaker: model?.engine === "kokoro" ? "af_heart" : "default",
          language: "en",
          speed: 1,
          seed: 42817 + index,
          output_format: "wav",
          voice_name: "Comparison voice",
        };
      }) });
      await applyRun(created);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally { setBusy(false); }
  }

  async function review(changes: Record<string, unknown>) {
    if (!comparison) return;
    try {
      await applyRun(await updateComparisonReview(comparison.id, changes));
      setError(undefined);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  function seek(next: number) {
    const normalized = Math.max(0, Math.min(1, next));
    Object.values(audioRefs.current).forEach((audio) => { if (audio && Number.isFinite(audio.duration)) audio.currentTime = normalized * audio.duration; });
    setProgress(normalized);
  }

  async function toggleTake(id: string) {
    const target = audioRefs.current[id];
    if (!target) return;
    Object.entries(audioRefs.current).forEach(([otherId, audio]) => { if (otherId !== id) audio?.pause(); });
    if (!target.paused) { target.pause(); return; }
    target.currentTime = progress * (Number.isFinite(target.duration) ? target.duration : 0);
    try { await target.play(); setPlayingId(id); } catch { setError("This comparison audio could not be played."); }
  }

  async function openRun(id: string) {
    const existing = runs.find((run) => run.id === id);
    try { await applyRun(bootstrap.runtime === "tauri" ? await getComparison(id) : existing!); }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  return (
    <div className="page compare-page">
      <PageHeader title="Compare" subtitle="Render a blind matrix of takes, seek them together, and preserve every review decision." actions={<><button className="button button-primary" type="button" disabled={running || !script.trim() || !models.length} onClick={() => void renderTakes()}>{running ? <LoaderCircle className="spin" size={14} /> : <Play size={14} />}{running ? "Rendering" : "Render takes"}</button>{running ? <button className="button button-secondary danger-button" type="button" onClick={() => comparison && void cancelComparison(comparison.id)}><Pause size={14} />Cancel</button> : null}</>} />
      <Panel className="compare-setup" ariaLabel="Comparison setup">
        <div className="compare-setup-bar"><Segmented label="Take count" value={String(takeCount)} onChange={(value) => setTakeCount(Number(value))} options={[{ value: "2", label: "2 takes" }, { value: "3", label: "3 takes" }, { value: "4", label: "4 takes" }]} />{runs.length ? <SelectField label="Saved run" value={comparison?.id ?? ""} onChange={(id) => void openRun(id)} options={[{ value: "", label: "Recent comparisons" }, ...runs.map((run) => ({ value: run.id, label: `${run.status} / ${run.script.slice(0, 42)}` }))]} /> : null}</div>
        <textarea aria-label="Comparison script" value={script} onChange={(event) => setScript(event.target.value)} />
        <div className="compare-model-grid">{takeModels.slice(0, takeCount).map((modelId, index) => <SelectField key={index} label={`Take ${String.fromCharCode(65 + index)}`} value={modelId} onChange={(value) => setTakeModels((current) => current.map((model, modelIndex) => modelIndex === index ? value : model))} options={models.map((model) => ({ value: model.model_id, label: model.model_id }))} />)}</div>
        <span>Shared script / independent deterministic seeds / high-priority shared scheduler</span>{error ? <StatusText tone="danger">{error}</StatusText> : null}
      </Panel>
      {comparison ? <>
        <div className={`compare-grid compare-grid-${comparison.takes.length}`} aria-label="Comparison takes">{comparison.takes.map((take) => {
          const result = take.result;
          const hidden = comparison.blind && !comparison.revealed;
          return <Panel className={`compare-side ${comparison.winner_take_id === take.id ? "is-winner" : ""}`} key={take.id} ariaLabel={`Take ${take.label}`}>
            <div className="compare-heading"><span>{take.label}</span><div><strong>{hidden ? "Identity hidden" : take.request.model_id || result?.model_id || "Legacy take"}</strong><StatusText tone={take.status === "completed" ? "success" : take.status === "failed" ? "danger" : "warning"}>{take.status}</StatusText></div></div>
            {result ? <>
              <div className="mini-waveform">{result.waveform.slice(0, 64).map((height, index) => <i key={index} className="is-live" style={{ height: Math.max(3, Number(height) * 24), opacity: index / Math.max(1, result.waveform.length) <= progress ? 1 : .35 }} />)}</div>
              <div className="compare-player"><audio ref={(node) => { audioRefs.current[take.id] = node; }} src={urls[take.id]} preload="metadata" onTimeUpdate={(event) => { if (playingId === take.id && event.currentTarget.duration) setProgress(event.currentTarget.currentTime / event.currentTarget.duration); }} onPause={() => setPlayingId((id) => id === take.id ? undefined : id)} onEnded={() => setPlayingId(undefined)} /><button className="icon-button" title={`${playingId === take.id ? "Pause" : "Play"} take ${take.label}`} type="button" disabled={!urls[take.id]} onClick={() => void toggleTake(take.id)}>{playingId === take.id ? <Pause size={12} /> : <Play size={12} />}</button><button className="compare-scrubber" type="button" aria-label={`Seek take ${take.label}`} onClick={(event) => { const box = event.currentTarget.getBoundingClientRect(); seek((event.clientX - box.left) / box.width); }}><i style={{ width: `${progress * 100}%` }} /></button><span>{result.duration_seconds.toFixed(1)} s</span></div>
              <dl className="compact-definition-list"><div><dt>RTF</dt><dd>{result.rtf.toFixed(3)}x</dd></div><div><dt>Peak VRAM</dt><dd>{result.vram_peak_mb.toFixed(0)} MB</dd></div><div><dt>Seed</dt><dd>{take.request.seed}</dd></div></dl>
              <div className="take-rating" aria-label={`Rate take ${take.label}`}>{[1,2,3,4,5].map((rating) => <button className="icon-button" title={`${rating} star${rating === 1 ? "" : "s"}`} type="button" key={rating} onClick={() => void review({ take_id: take.id, rating })}><Star size={12} fill={(take.rating ?? 0) >= rating ? "currentColor" : "none"} /></button>)}<button className="icon-button" title={take.favorite ? "Remove favorite" : "Favorite take"} type="button" onClick={() => void review({ take_id: take.id, favorite: !take.favorite })}><Star size={13} fill={take.favorite ? "currentColor" : "none"} /></button></div>
              <textarea className="take-notes" aria-label={`Notes for take ${take.label}`} placeholder="Listening notes" defaultValue={take.notes} onBlur={(event) => { if (event.currentTarget.value !== take.notes) void review({ take_id: take.id, notes: event.currentTarget.value }); }} />
              <div className="take-actions"><button className="button button-secondary" type="button" onClick={() => void review({ winner_take_id: take.id, tie: false })}><Trophy size={12} />Winner</button><button className="button button-secondary" type="button" onClick={() => void review({ promoted_take_id: take.id })}><Check size={12} />Promote</button></div>
            </> : <div className="compare-pending"><LoaderCircle className={running ? "spin" : ""} size={15} /><span>{take.error ?? (running ? "Waiting for scheduler" : "No artifact")}</span></div>}
          </Panel>;
        })}</div>
        <Panel className="compare-decision" ariaLabel="Comparison decision"><div><span className="section-label">Review</span><StatusText tone={comparison.status === "completed" ? "success" : comparison.status === "partial" ? "warning" : "muted"}>{comparison.status}</StatusText></div><button className="button button-secondary" type="button" aria-pressed={comparison.tie} onClick={() => void review({ tie: true })}>Tie</button><button className="button button-secondary" type="button" disabled={comparison.revealed} onClick={() => void review({ revealed: true })}><Eye size={13} />{comparison.revealed ? "Identities revealed" : "Reveal identities"}</button><StatusText tone={comparison.promoted_take_id ? "success" : "muted"}>{comparison.promoted_take_id ? `Take ${comparison.takes.find((take) => take.id === comparison.promoted_take_id)?.label} promoted to History` : comparison.tie ? "Marked as a tie" : "Rate first, then choose a winner and promotion"}</StatusText></Panel>
      </> : <EmptyState title="No comparison rendered" detail="Choose two to four takes and render a blind matrix." />}
    </div>
  );
}

export function HistoryView({ history, onChange }: { history: HistoryItem[]; onChange: (history: HistoryItem[]) => void }) {
  const [query, setQuery] = useState("");
  const [modelFilter, setModelFilter] = useState("");
  const [voiceFilter, setVoiceFilter] = useState("");
  const [stateFilter, setStateFilter] = useState("");
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [facets, setFacets] = useState(() => ({ models: [...new Set(history.map((item) => item.model_id))].sort(), voices: [...new Set(history.map((item) => item.voice))].sort() }));
  const [activeId, setActiveId] = useState<string>();
  const [loadingId, setLoadingId] = useState<string>();
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackError, setPlaybackError] = useState<{ id: string; message: string }>();
  const [rowNotice, setRowNotice] = useState<{ id: string; message: string }>();
  const [searching, setSearching] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);
  const objectUrlRef = useRef<string | undefined>(undefined);
  const requestRef = useRef(0);
  const filters = useMemo<HistoryFilters>(() => ({ ...(modelFilter ? { model_id: modelFilter } : {}), ...(voiceFilter ? { voice: voiceFilter } : {}), ...(stateFilter ? { artifact_state: stateFilter as HistoryFilters["artifact_state"] } : {}), ...(favoritesOnly ? { favorite: true } : {}) }), [modelFilter, voiceFilter, stateFilter, favoritesOnly]);

  useEffect(() => setFacets((current) => ({ models: [...new Set([...current.models, ...history.map((item) => item.model_id)])].sort(), voices: [...new Set([...current.voices, ...history.map((item) => item.voice)])].sort() })), [history]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSearching(true);
      listHistory(query, filters)
        .then(onChange)
        .catch(() => undefined)
        .finally(() => setSearching(false));
    }, 180);
    return () => window.clearTimeout(timer);
  }, [query, filters]);

  useEffect(() => () => {
    requestRef.current += 1;
    audioRef.current?.pause();
    if (objectUrlRef.current?.startsWith("blob:")) URL.revokeObjectURL(objectUrlRef.current);
  }, []);

  async function refreshHistory() {
    onChange(await listHistory(query, filters));
  }

  async function toggleHistoryPlayback(item: HistoryItem) {
    const audio = audioRef.current;
    if (!audio || !item.audio_path) return;
    if (activeId === item.id && audio.src) {
      if (!audio.paused) {
        audio.pause();
        return;
      }
      try {
        await audio.play();
        setPlaybackError(undefined);
      } catch (caught) {
        setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : "Audio playback failed" });
      }
      return;
    }

    const requestId = ++requestRef.current;
    audio.pause();
    setIsPlaying(false);
    setLoadingId(item.id);
    setPlaybackError(undefined);
    try {
      const url = await loadGeneratedAudio(item.audio_path);
      if (requestId !== requestRef.current) {
        if (url.startsWith("blob:")) URL.revokeObjectURL(url);
        return;
      }
      if (objectUrlRef.current?.startsWith("blob:")) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = url;
      audio.src = url;
      audio.load();
      setActiveId(item.id);
      await audio.play();
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : "Audio playback failed" });
      setActiveId(undefined);
    } finally {
      if (requestId === requestRef.current) setLoadingId(undefined);
    }
  }

  async function remove(item: HistoryItem) {
    if (!window.confirm(`Delete “${item.title}” and its generated audio?`)) return;
    try {
      if (await deleteHistoryItem(item.id, true)) await refreshHistory();
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function copyText(item: HistoryItem) {
    try {
      await navigator.clipboard.writeText(item.text);
    } catch {
      setPlaybackError({ id: item.id, message: "Could not copy text to the clipboard" });
    }
  }

  async function toggleFavorite(item: HistoryItem) {
    try {
      await updateHistoryMetadata(item.id, { favorite: !item.favorite });
      await refreshHistory();
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function editNotes(item: HistoryItem) {
    const notes = window.prompt("Generation notes", item.notes ?? "");
    if (notes === null) return;
    try {
      await updateHistoryMetadata(item.id, { notes });
      await refreshHistory();
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function regenerate(item: HistoryItem) {
    setLoadingId(item.id);
    try {
      const request = await getHistoryRequest(item.id);
      await synthesizeSpeech({ ...request, title: `${item.title} rerun` });
      await refreshHistory();
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    } finally {
      setLoadingId(undefined);
    }
  }

  async function duplicate(item: HistoryItem) {
    setLoadingId(item.id);
    try {
      await duplicateHistoryItem(item.id);
      await refreshHistory();
      setPlaybackError(undefined);
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    } finally { setLoadingId(undefined); }
  }

  async function exportCopy(item: HistoryItem) {
    try {
      const receipt = await exportHistoryItem(item);
      if (receipt) {
        setPlaybackError(undefined);
        setRowNotice({ id: item.id, message: `Exported ${receipt.format.toUpperCase()} copy` });
      }
    } catch (caught) {
      setPlaybackError({ id: item.id, message: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  return (
    <div className="page">
      <PageHeader title="History" subtitle="Reopen, audition, and export generations made on this machine." />
      <div className="history-toolbar"><label className="search-control"><Search size={14} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search scripts, voices, and models..." /></label><SelectField label="Model filter" value={modelFilter} onChange={setModelFilter} options={[{ value: "", label: "All models" }, ...facets.models.map((model) => ({ value: model, label: model.split("/").at(-1) ?? model }))]} /><SelectField label="Voice filter" value={voiceFilter} onChange={setVoiceFilter} options={[{ value: "", label: "All voices" }, ...facets.voices.map((voice) => ({ value: voice, label: voice }))]} /><SelectField label="Artifact filter" value={stateFilter} onChange={setStateFilter} options={[{ value: "", label: "All artifacts" }, { value: "available", label: "Available" }, { value: "unavailable", label: "Unavailable" }]} /><button className="button button-secondary history-favorite-filter" type="button" aria-pressed={favoritesOnly} onClick={() => setFavoritesOnly((value) => !value)}><Star size={12} fill={favoritesOnly ? "currentColor" : "none"} />Favorites</button><StatusText tone="muted">{searching ? "Searching local database..." : `${history.length} matching generations${history.length === 500 ? " shown" : ""}`}</StatusText></div>
      <Panel className="table-panel">
        {history.length ? (
          <div className="table-scroll">
            <table className="data-table history-table">
              <thead><tr><th>Generation</th><th>Voice</th><th>Model</th><th>Duration</th><th>RTF</th><th aria-label="Actions" /></tr></thead>
              <tbody>{history.map((item) => {
                const playing = activeId === item.id && isPlaying;
                const loading = loadingId === item.id;
                const artifactUnavailable = !item.audio_path || item.missing || item.artifact_state === "missing" || item.artifact_state === "modified";
                const artifactMessage = item.artifact_state === "modified"
                  ? "Audio file changed on disk"
                  : artifactUnavailable ? "Audio file is missing" : undefined;
                return (
                  <tr key={item.id}>
                    <td>
                      <strong>{item.favorite ? <Star aria-label="Favorite" fill="currentColor" size={11} /> : null}{item.title}</strong>
                      <small className={playbackError?.id === item.id || artifactMessage ? "history-playback-error" : rowNotice?.id === item.id ? "history-row-notice" : undefined}>{playbackError?.id === item.id ? playbackError.message : rowNotice?.id === item.id ? rowNotice.message : (artifactMessage ?? item.notes) || new Date(item.created_at).toLocaleString()}</small>
                    </td>
                    <td>{item.voice}</td>
                    <td className="muted-cell">{item.model_id.split("/").at(-1)}</td>
                    <td className="mono-cell">{item.duration_seconds.toFixed(1)} s</td>
                    <td className="mono-cell">{item.rtf.toFixed(2)}x</td>
                    <td>
                      <div className="history-actions"><button className="icon-button" title={artifactMessage ?? (playing ? "Pause generation" : "Play generation")} type="button" disabled={!item.audio_path || artifactUnavailable || loading} onClick={() => void toggleHistoryPlayback(item)}>
                        {loading ? <LoaderCircle className="spin" size={12} /> : playing ? <Pause fill="currentColor" size={12} /> : <Play fill="currentColor" size={12} />}
                      </button><RowActionMenu label={`More actions for ${item.title}`} actions={[
                        { label: item.favorite ? "Remove favorite" : "Add favorite", icon: <Star fill={item.favorite ? "currentColor" : "none"} size={12} />, onSelect: () => toggleFavorite(item) },
                        { label: "Edit notes", icon: <NotebookPen size={12} />, onSelect: () => editNotes(item) },
                        { label: "Regenerate", icon: <RefreshCw size={12} />, disabled: loading, onSelect: () => regenerate(item) },
                        { label: "Duplicate artifact", icon: <Copy size={12} />, disabled: loading || artifactUnavailable, onSelect: () => duplicate(item) },
                        { label: "Export copy", icon: <Download size={12} />, disabled: artifactUnavailable, onSelect: () => exportCopy(item) },
                        { label: "Reveal in folder", icon: <FolderOpen size={12} />, disabled: !item.audio_path || artifactUnavailable, onSelect: () => { if (item.audio_path) void revealItemInDir(item.audio_path); } },
                        { label: "Copy script", icon: <Copy size={12} />, onSelect: () => copyText(item) },
                        { label: "Delete", icon: <Trash2 size={12} />, danger: true, onSelect: () => remove(item) },
                      ]} /></div>
                    </td>
                  </tr>
                );
              })}</tbody>
            </table>
          </div>
        ) : <EmptyState title="No matching generations" detail="Generated audio will appear here automatically." />}
        <audio ref={audioRef} className="visually-hidden" preload="metadata" onPlay={() => setIsPlaying(true)} onPause={() => setIsPlaying(false)} onEnded={() => setIsPlaying(false)} onError={() => activeId && setPlaybackError({ id: activeId, message: "Generated audio could not be decoded" })} />
      </Panel>
    </div>
  );
}

export function SettingsView({ bootstrap, settings, onSetting }: { bootstrap: BootstrapState; settings: ApplicationSettings; onSetting: <K extends keyof ApplicationSettings>(key: K, value: ApplicationSettings[K]) => void }) {
  const [api, setApi] = useState<DeveloperApiState>({ running: false });
  const [apiBusy, setApiBusy] = useState(false);
  const [apiError, setApiError] = useState<string>();

  useEffect(() => { void getDeveloperApiStatus().then(setApi).catch(() => undefined); }, []);

  async function toggleApi() {
    setApiBusy(true); setApiError(undefined);
    try {
      if (api.running) { await stopDeveloperApi(); setApi({ running: false }); }
      else setApi(await startDeveloperApi());
    } catch (caught) { setApiError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setApiBusy(false); }
  }

  return (
    <div className="page settings-page">
      <PageHeader title="Settings" subtitle="Tune the desktop runtime, storage, and visual appearance." />
      <div className="settings-columns">
        <Panel className="settings-section">
          <div className="settings-title"><SlidersHorizontal size={15} /><div><h2>Appearance</h2><p>Compact in both themes, with no blue or purple.</p></div></div>
          <span className="field-label standalone-label">Color mode</span>
          <Segmented label="Color mode" value={settings.theme} onChange={(theme) => onSetting("theme", theme)} options={[{ value: "dark", label: "Dark" }, { value: "light", label: "Cream light" }]} />
          <label className="toggle-row"><span><strong>Dense tables</strong><small>Keep rows compact across the workspace.</small></span><input type="checkbox" checked={settings.dense_tables} onChange={(event) => onSetting("dense_tables", event.target.checked)} /></label>
          <label className="toggle-row"><span><strong>Reduced motion</strong><small>Limit interface animation.</small></span><input type="checkbox" checked={settings.reduced_motion} onChange={(event) => onSetting("reduced_motion", event.target.checked)} /></label>
        </Panel>
        <Panel className="settings-section">
          <div className="settings-title"><Gauge size={15} /><div><h2>Local runtime</h2><p>Hardware and engine health reported by the desktop bridge.</p></div></div>
          <dl className="compact-definition-list settings-facts"><div><dt>GPU</dt><dd>{bootstrap.system.gpu_name}</dd></div><div><dt>CUDA</dt><dd><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "Ready" : "Unavailable"}</StatusText></dd></div><div><dt>Python engines</dt><dd><StatusText tone={bootstrap.system.python_ready ? "success" : "danger"}>{bootstrap.system.python_ready ? "Ready" : "Missing"}</StatusText></dd></div><div><dt>Driver</dt><dd>{bootstrap.system.driver_version || "Unknown"}</dd></div></dl>
        </Panel>
        <Panel className="settings-section settings-storage">
          <div className="settings-title"><FolderOpen size={15} /><div><h2>Storage</h2><p>Models, voice references, and exports stay local.</p></div></div>
          <CompactField label="Export directory"><div className="path-field"><span>{bootstrap.export_dir}</span></div></CompactField>
          <div className="storage-summary"><div><strong>{bootstrap.installed.length}</strong><span>Installed models</span></div><div><strong>{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB</strong><span>GPU memory</span></div><div><strong><Check size={15} /></strong><span>Local-only mode</span></div></div>
        </Panel>
        <Panel className="settings-section settings-api">
          <div className="settings-title"><Server size={15} /><div><h2>Developer API</h2><p>Explicit, token-protected access on this machine only.</p></div></div>
          <dl className="compact-definition-list settings-facts"><div><dt>Status</dt><dd><StatusText tone={api.running ? "success" : "muted"}>{api.running ? "Listening" : "Stopped"}</StatusText></dd></div><div><dt>Binding</dt><dd>{api.base_url ?? "127.0.0.1:17843"}</dd></div><div><dt>Compatibility</dt><dd>OpenAI audio/speech</dd></div></dl>
          {api.running && api.token ? <div className="api-token"><KeyRound size={13} /><code>{api.token}</code><button className="icon-button" title="Copy API token" type="button" onClick={() => void navigator.clipboard.writeText(api.token ?? "")}><Copy size={12} /></button></div> : null}
          {apiError ? <StatusText tone="danger">{apiError}</StatusText> : null}
          <button className={`button ${api.running ? "button-secondary danger-button" : "button-primary"}`} type="button" disabled={apiBusy || bootstrap.runtime !== "tauri"} onClick={() => void toggleApi()}>{apiBusy ? <LoaderCircle className="spin" size={13} /> : <Server size={13} />}{apiBusy ? "Updating" : api.running ? "Stop local API" : "Start local API"}</button>
        </Panel>
      </div>
    </div>
  );
}

export function AboutView({ bootstrap }: { bootstrap: BootstrapState }) {
  return (
    <div className="page about-page">
      <PageHeader title="About" subtitle="Application identity and local runtime details." />
      <section className="about-identity" aria-labelledby="about-product-name">
        <BrandMark className="about-mark" />
        <div>
          <BrandLockup className="about-lockup" />
          <p id="about-product-name">Open-source local voice studio</p>
        </div>
        <span className="about-version">Version 0.3.0</span>
      </section>
      <div className="about-details">
        <Panel className="about-section" ariaLabel="Application details">
          <span className="section-label">Application</span>
          <dl className="compact-definition-list settings-facts">
            <div><dt>Desktop shell</dt><dd>Tauri 2</dd></div>
            <div><dt>Interface</dt><dd>React 19</dd></div>
            <div><dt>Inference</dt><dd>Local Python worker</dd></div>
            <div><dt>Updates</dt><dd>Automatic release checks</dd></div>
            <div><dt>Network fallback</dt><dd>None</dd></div>
          </dl>
        </Panel>
        <Panel className="about-section" ariaLabel="Runtime details">
          <span className="section-label">This machine</span>
          <dl className="compact-definition-list settings-facts">
            <div><dt>GPU</dt><dd>{bootstrap.system.gpu_name}</dd></div>
            <div><dt>VRAM</dt><dd>{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB</dd></div>
            <div><dt>CUDA</dt><dd><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "Ready" : "Unavailable"}</StatusText></dd></div>
            <div><dt>Installed models</dt><dd>{bootstrap.installed.length}</dd></div>
          </dl>
        </Panel>
      </div>
      <footer className="about-footer"><span>soundAr</span><span>Local only</span><span>Open-source application</span></footer>
    </div>
  );
}
