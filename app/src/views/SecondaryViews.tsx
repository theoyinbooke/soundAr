import { Activity, ArrowLeft, AudioWaveform, Check, CircleStop, Copy, Download, Eye, FolderOpen, Gauge, HardDrive, KeyRound, LoaderCircle, Mic, MonitorCog, NotebookPen, Palette, Pause, Play, RefreshCw, Search, Server, Settings2, SlidersHorizontal, Star, Trophy, Trash2, Volume2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { ApplicationSettings, AudioInputDevice, AudioOutputDevice, AudioPlaybackState, AudioRecordingState, BootstrapState, ComparisonRecord, DeveloperApiState, HistoryItem, UpdateCheckStatus } from "../types";
import { CompactAudioPlayer, CompactField, EmptyState, MetricStrip, PageHeader, Panel, RowActionMenu, Segmented, SelectField, StatusText } from "../components/ui";
import { cancelComparison, createComparison, deleteHistoryItem, duplicateHistoryItem, exportHistoryItem, generateMusic, getAudioPlaybackStatus, getAudioRecordingStatus, getComparison, getDeveloperApiStatus, getHistoryRequest, listAudioInputDevices, listAudioOutputDevices, listHistory, loadGeneratedAudio, loadTranscriptionAudio, startAudioPlayback, startAudioRecording, startDeveloperApi, stopAudioPlayback, stopAudioRecording, stopDeveloperApi, synthesizeSpeech, transcribeAudio, updateComparisonReview, updateHistoryMetadata } from "../lib/bridge";
import { canSynthesizeWithoutReference, qualifiedModels } from "../lib/capabilities";

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
  return (
    <div className="page live-page">
      <PageHeader title="Live" subtitle="Capture local microphone audio, monitor input, review it, and transcribe without a cloud service." actions={<button className={`button ${recording.recording ? "button-secondary danger-button" : "button-primary"}`} type="button" disabled={!devices.length} onClick={() => void toggleRecording()}>{recording.recording ? <CircleStop size={14} /> : <Mic size={14} />}{recording.recording ? "Stop capture" : "Record"}</button>} />
      <MetricStrip metrics={[{ value: `${(recording.duration_seconds ?? 0).toFixed(1)} s`, label: "Capture" }, { value: `${(recording.speech_seconds ?? 0).toFixed(1)} s`, label: "Detected speech" }, { value: `${Math.round(peak * 100)}%`, label: "Input peak" }, { value: String(recording.dropped_frames ?? 0), label: "Dropped frames" }]} />
      <div className="live-layout">
        <Panel className="live-stage" ariaLabel="Live voice session">
          <div className={`live-orb ${recording.recording ? "is-active" : ""}`}><Activity size={22} /></div>
          <div><h2>{recording.recording ? recording.speech_active ? "Voice detected" : recording.speech_detected ? "Listening for more" : "Listening" : recording.audio_path ? "Capture ready" : "Microphone ready"}</h2><p>{recording.recording ? recording.device_name : recording.audio_path ? `${(recording.duration_seconds ?? 0).toFixed(1)} seconds captured as a managed WAV${recording.stop_reason === "silence" ? " after silence was detected" : ""}.` : "Select an input, then start a visible local recording."}</p></div>
          <div className="input-meter" aria-label={`Input level ${Math.round(peak * 100)} percent`}><i style={{ width: `${Math.round(peak * 100)}%` }} /></div>
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
              <div className="mini-waveform"><i className="is-live" style={{ width: `${progress * 100}%` }} /></div>
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

export function HistoryView({ history, onChange, selectedId }: { history: HistoryItem[]; onChange: (history: HistoryItem[]) => void; selectedId?: string }) {
  const [activeId, setActiveId] = useState<string>();
  const [loadingId, setLoadingId] = useState<string>();
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackError, setPlaybackError] = useState<{ id: string; message: string }>();
  const [rowNotice, setRowNotice] = useState<{ id: string; message: string }>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const objectUrlRef = useRef<string | undefined>(undefined);
  const requestRef = useRef(0);

  useEffect(() => () => {
    requestRef.current += 1;
    audioRef.current?.pause();
    if (objectUrlRef.current?.startsWith("blob:")) URL.revokeObjectURL(objectUrlRef.current);
  }, []);

  async function refreshHistory() {
    onChange(await listHistory());
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
      if (item.generation_kind === "music") {
        const request = await getHistoryRequest(item.id);
        if (!("prompt" in request)) throw new Error("The stored music request is invalid.");
        const lyrics = request.lyrics?.trim();
        await navigator.clipboard.writeText([
          `Music direction:\n${request.prompt}`,
          lyrics ? `Lyrics or text to sing:\n${lyrics}` : "",
        ].filter(Boolean).join("\n\n"));
      } else {
        await navigator.clipboard.writeText(item.text);
      }
    } catch {
      setPlaybackError({ id: item.id, message: "Could not copy generation text to the clipboard" });
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
      if (item.generation_kind === "music") {
        if (!("prompt" in request)) throw new Error("The stored music request is invalid.");
        await generateMusic({ ...request, title: `${item.title} rerun` });
      } else {
        if (!("text" in request)) throw new Error("The stored speech request is invalid.");
        await synthesizeSpeech({ ...request, title: `${item.title} rerun` });
      }
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

  const selected = history.find((item) => item.id === selectedId) ?? history[0];
  const selectedPlaying = Boolean(selected && activeId === selected.id && isPlaying);
  const selectedLoading = Boolean(selected && loadingId === selected.id);
  const selectedUnavailable = Boolean(selected && (!selected.audio_path || selected.missing || selected.artifact_state === "missing" || selected.artifact_state === "modified"));
  const selectedArtifactMessage = selected?.artifact_state === "modified"
    ? "Audio file changed on disk"
    : selectedUnavailable ? "Audio file is missing" : undefined;

  return (
    <div className="page history-page">
      <PageHeader title="History" subtitle="Reopen, audition, and export generations made on this machine." />
      <Panel className={`history-workspace history-detail-only${selected ? "" : " is-empty"}`} ariaLabel="Generation history">
        <section className="history-detail" aria-label="Generation details">
          {selected ? <>
            <header className="history-detail-header">
              <div><span className="section-label">{selected.generation_kind === "music" ? "Music generation" : "Voice generation"}</span><h2>{selected.title}</h2><p>{new Date(selected.created_at).toLocaleString()}</p></div>
              <div className="history-detail-actions">
                <button className="icon-button" title={selected.favorite ? "Remove favorite" : "Add favorite"} type="button" onClick={() => void toggleFavorite(selected)}><Star fill={selected.favorite ? "currentColor" : "none"} size={13} /></button>
                <RowActionMenu label={`More actions for ${selected.title}`} actions={[
                  { label: selected.favorite ? "Remove favorite" : "Add favorite", icon: <Star fill={selected.favorite ? "currentColor" : "none"} size={12} />, onSelect: () => toggleFavorite(selected) },
                  { label: "Edit notes", icon: <NotebookPen size={12} />, onSelect: () => editNotes(selected) },
                  { label: "Regenerate", icon: <RefreshCw size={12} />, disabled: selectedLoading, onSelect: () => regenerate(selected) },
                  { label: "Duplicate artifact", icon: <Copy size={12} />, disabled: selectedLoading || selectedUnavailable, onSelect: () => duplicate(selected) },
                  { label: "Export copy", icon: <Download size={12} />, disabled: selectedUnavailable, onSelect: () => exportCopy(selected) },
                  { label: "Reveal in folder", icon: <FolderOpen size={12} />, disabled: !selected.audio_path || selectedUnavailable, onSelect: () => { if (selected.audio_path) void revealItemInDir(selected.audio_path); } },
                  { label: selected.generation_kind === "music" ? "Copy direction and lyrics" : "Copy script", icon: <Copy size={12} />, onSelect: () => copyText(selected) },
                  { label: "Delete", icon: <Trash2 size={12} />, danger: true, onSelect: () => remove(selected) },
                ]} />
              </div>
            </header>
            <div className="history-player-row">
              <button className="history-play-button" title={selectedArtifactMessage ?? (selectedPlaying ? "Pause generation" : "Play generation")} type="button" disabled={!selected.audio_path || selectedUnavailable || selectedLoading} onClick={() => void toggleHistoryPlayback(selected)}>{selectedLoading ? <LoaderCircle className="spin" size={15} /> : selectedPlaying ? <Pause fill="currentColor" size={15} /> : <Play fill="currentColor" size={15} />}</button>
              <span><strong>{selectedArtifactMessage ?? (playbackError?.id === selected.id ? playbackError.message : rowNotice?.id === selected.id ? rowNotice.message : "Ready to play")}</strong><small>{selected.model_id.split("/").at(-1)} · {selected.voice}</small></span>
              <strong className="mono-cell">{selected.duration_seconds.toFixed(1)} s</strong>
            </div>
            <dl className="history-detail-facts"><div><dt>Model</dt><dd>{selected.model_id.split("/").at(-1)}</dd></div><div><dt>Voice</dt><dd>{selected.generation_kind === "music" ? "Music" : selected.voice}</dd></div><div><dt>RTF</dt><dd>{selected.rtf.toFixed(2)}x</dd></div><div><dt>Peak VRAM</dt><dd>{selected.vram_peak_mb.toFixed(0)} MB</dd></div></dl>
            <section className="history-script"><span className="section-label">{selected.generation_kind === "music" ? "Direction" : "Script"}</span><p>{selected.text}</p></section>
            {selected.notes ? <section className="history-notes"><span className="section-label">Notes</span><p>{selected.notes}</p></section> : null}
            <div className="history-primary-actions"><button className="button button-secondary" type="button" disabled={selectedLoading} onClick={() => void regenerate(selected)}><RefreshCw size={12} />Regenerate</button><button className="button button-primary" type="button" disabled={selectedUnavailable} onClick={() => void exportCopy(selected)}><Download size={12} />Export copy</button></div>
          </> : <EmptyState title="No generations yet" detail="Generated speech and music will appear in the Recent list in the sidebar." />}
        </section>
        <audio ref={audioRef} className="visually-hidden" preload="metadata" onPlay={() => setIsPlaying(true)} onPause={() => setIsPlaying(false)} onEnded={() => setIsPlaying(false)} onError={() => activeId && setPlaybackError({ id: activeId, message: "Generated audio could not be decoded" })} />
      </Panel>
    </div>
  );
}

function UpdateCheckButton({ status, onCheck }: { status: UpdateCheckStatus; onCheck: () => void }) {
  return (
    <button className="button button-secondary" type="button" disabled={status.phase === "checking"} onClick={onCheck}>
      {status.phase === "checking" ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <RefreshCw aria-hidden="true" size={13} />}
      {status.phase === "checking" ? "Checking..." : "Check for updates"}
    </button>
  );
}

function UpdateCheckFeedback({ status }: { status: UpdateCheckStatus }) {
  if (!status.message || status.phase === "idle") return null;
  const tone = status.phase === "current"
    ? "success"
    : status.phase === "available" || status.phase === "checking"
      ? "warning"
      : status.phase === "error"
        ? "danger"
        : "muted";
  return <div className="update-check-feedback" role="status" aria-live="polite"><StatusText tone={tone}>{status.message}</StatusText></div>;
}

type SettingsCategory = "general" | "appearance" | "audio" | "storage" | "runtime" | "developer";

export function SettingsView({ bootstrap, settings, onSetting, updateCheck, onCheckForUpdates, onBack }: { bootstrap: BootstrapState; settings: ApplicationSettings; onSetting: <K extends keyof ApplicationSettings>(key: K, value: ApplicationSettings[K]) => void; updateCheck: UpdateCheckStatus; onCheckForUpdates: () => void; onBack: () => void }) {
  const [api, setApi] = useState<DeveloperApiState>({ running: false });
  const [apiBusy, setApiBusy] = useState(false);
  const [apiError, setApiError] = useState<string>();
  const [category, setCategory] = useState<SettingsCategory>("general");
  const [query, setQuery] = useState("");

  useEffect(() => { void getDeveloperApiStatus().then(setApi).catch(() => undefined); }, []);

  async function toggleApi() {
    setApiBusy(true); setApiError(undefined);
    try {
      if (api.running) { await stopDeveloperApi(); setApi({ running: false }); }
      else setApi(await startDeveloperApi());
    } catch (caught) { setApiError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setApiBusy(false); }
  }

  const categories: Array<{ group: string; key: SettingsCategory; label: string; icon: typeof Settings2 }> = [
    { group: "Application", key: "general", label: "General", icon: Settings2 },
    { group: "Application", key: "appearance", label: "Appearance", icon: Palette },
    { group: "Application", key: "audio", label: "Audio", icon: AudioWaveform },
    { group: "Local engine", key: "storage", label: "Models & storage", icon: HardDrive },
    { group: "Local engine", key: "runtime", label: "Runtime & updates", icon: MonitorCog },
    { group: "Advanced", key: "developer", label: "Developer API", icon: Server },
  ];
  const filteredCategories = categories.filter((item) => `${item.group} ${item.label}`.toLowerCase().includes(query.trim().toLowerCase()));

  return (
    <div className="settings-shell">
      <aside className="settings-navigation">
        <button className="settings-back" type="button" onClick={onBack}><ArrowLeft aria-hidden="true" size={16} />Back to soundAr</button>
        <label className="settings-search"><Search aria-hidden="true" size={15} /><span className="visually-hidden">Search settings</span><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search settings…" /></label>
        <nav aria-label="Settings categories">
          {["Application", "Local engine", "Advanced"].map((group) => {
            const items = filteredCategories.filter((item) => item.group === group);
            if (!items.length) return null;
            return <div className="settings-nav-group" key={group}><span>{group}</span>{items.map((item) => {
              const Icon = item.icon;
              return <button key={item.key} type="button" className={category === item.key ? "is-active" : ""} aria-current={category === item.key ? "page" : undefined} onClick={() => setCategory(item.key)}><Icon aria-hidden="true" size={16} /><span>{item.label}</span></button>;
            })}</div>;
          })}
          {!filteredCategories.length ? <p className="settings-no-results">No settings match “{query}”.</p> : null}
        </nav>
      </aside>

      <section className="settings-content">
        <div className="settings-content-inner">
          {category === "general" ? <>
            <header className="settings-page-heading"><h1>General</h1><p>Manage soundAr’s desktop behavior and local workspace defaults.</p></header>
            <section className="settings-block" aria-labelledby="general-behavior"><h2 id="general-behavior">Application</h2><div className="settings-group">
              <div className="settings-row"><span><strong>Local-first processing</strong><small>Speech, transcription, model files, and projects stay on this computer.</small></span><span className="settings-value success-value"><Check size={14} /> Active</span></div>
              <div className="settings-row"><span><strong>Installed version</strong><small>Desktop application and bundled runtime.</small></span><span className="settings-value">{__APP_VERSION__}</span></div>
              <div className="settings-row"><span><strong>Table density</strong><small>Choose how much information is visible in libraries and history.</small></span><Segmented label="Table density" value={settings.dense_tables ? "dense" : "comfortable"} onChange={(value) => onSetting("dense_tables", value === "dense")} options={[{ value: "comfortable", label: "Comfortable" }, { value: "dense", label: "Compact" }]} /></div>
            </div></section>
          </> : null}

          {category === "appearance" ? <>
            <header className="settings-page-heading"><h1>Appearance</h1><p>Choose how soundAr looks and moves across the desktop.</p></header>
            <section className="settings-block"><h2>Theme</h2><div className="settings-group">
              <div className="settings-row"><span><strong>Color mode</strong><small>Use a neutral light or dark desktop theme.</small></span><Segmented label="Color mode" value={settings.theme} onChange={(theme) => onSetting("theme", theme)} options={[{ value: "light", label: "Light" }, { value: "dark", label: "Dark" }]} /></div>
              <label className="settings-row"><span><strong>Reduced motion</strong><small>Minimize non-essential transitions and interface animation.</small></span><input type="checkbox" checked={settings.reduced_motion} onChange={(event) => onSetting("reduced_motion", event.target.checked)} /></label>
            </div></section>
          </> : null}

          {category === "audio" ? <>
            <header className="settings-page-heading"><h1>Audio</h1><p>Review the local audio environment used for recording and generation.</p></header>
            <section className="settings-block"><h2>Processing</h2><div className="settings-group">
              <div className="settings-row"><span><strong>Inference mode</strong><small>All enabled engines run through the local soundAr worker.</small></span><span className="settings-value">Local</span></div>
              <div className="settings-row"><span><strong>GPU acceleration</strong><small>Hardware acceleration is used by compatible installed engines.</small></span><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "Available" : "Unavailable"}</StatusText></div>
              <div className="settings-row"><span><strong>Python audio engines</strong><small>Required by model runtimes and audio processing tools.</small></span><StatusText tone={bootstrap.system.python_ready ? "success" : "danger"}>{bootstrap.system.python_ready ? "Ready" : "Needs setup"}</StatusText></div>
            </div></section>
          </> : null}

          {category === "storage" ? <>
            <header className="settings-page-heading"><h1>Models & storage</h1><p>Understand where soundAr keeps model assets and generated work.</p></header>
            <section className="settings-block"><h2>Storage</h2><div className="settings-group">
              <div className="settings-row settings-path-row"><span><strong>Export directory</strong><small>Generated audio is written to this local folder.</small></span><code>{bootstrap.export_dir}</code></div>
              <div className="settings-row"><span><strong>Installed models</strong><small>Models currently available to local engines.</small></span><span className="settings-value">{bootstrap.installed.length}</span></div>
              <div className="settings-row"><span><strong>GPU memory</strong><small>Total memory visible to the local runtime.</small></span><span className="settings-value">{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB</span></div>
            </div></section>
          </> : null}

          {category === "runtime" ? <>
            <header className="settings-page-heading"><h1>Runtime & updates</h1><p>Inspect the local engine and keep the installed application current.</p></header>
            <UpdateCheckFeedback status={updateCheck} />
            <section className="settings-block"><h2>Application updates</h2><div className="settings-group">
              <div className="settings-row"><span><strong>soundAr updates</strong><small>Check the signed release feed for a newer desktop build.</small></span><UpdateCheckButton status={updateCheck} onCheck={onCheckForUpdates} /></div>
            </div></section>
            <section className="settings-block"><h2>Local runtime</h2><div className="settings-group">
              <div className="settings-row"><span><strong>GPU</strong><small>{bootstrap.system.gpu_name}</small></span><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "CUDA ready" : "CPU mode"}</StatusText></div>
              <div className="settings-row"><span><strong>Driver</strong><small>Reported by the desktop bridge.</small></span><span className="settings-value">{bootstrap.system.driver_version || "Unknown"}</span></div>
              <div className="settings-row"><span><strong>Install type</strong><small>Current Linux distribution package.</small></span><span className="settings-value">{bootstrap.install_kind.toUpperCase()}</span></div>
            </div></section>
          </> : null}

          {category === "developer" ? <>
            <header className="settings-page-heading"><h1>Developer API</h1><p>Expose an explicit token-protected audio endpoint on this machine.</p></header>
            <section className="settings-block"><h2>Local endpoint</h2><div className="settings-group">
              <div className="settings-row"><span><strong>Status</strong><small>{api.base_url ?? "127.0.0.1:17843"} · OpenAI audio/speech compatibility</small></span><StatusText tone={api.running ? "success" : "muted"}>{api.running ? "Listening" : "Stopped"}</StatusText></div>
              {api.running && api.token ? <div className="settings-row"><span><strong>API token</strong><small>Keep this credential private.</small></span><div className="api-token"><KeyRound size={13} /><code>{api.token}</code><button className="icon-button" title="Copy API token" type="button" onClick={() => void navigator.clipboard.writeText(api.token ?? "")}><Copy size={12} /></button></div></div> : null}
              <div className="settings-row"><span><strong>{api.running ? "Stop local API" : "Start local API"}</strong><small>{bootstrap.runtime === "tauri" ? "The endpoint only listens after you explicitly enable it." : "Available in the installed desktop application."}</small></span><button className={`button ${api.running ? "button-secondary danger-button" : "button-primary"}`} type="button" disabled={apiBusy || bootstrap.runtime !== "tauri"} onClick={() => void toggleApi()}>{apiBusy ? <LoaderCircle className="spin" size={13} /> : <Server size={13} />}{apiBusy ? "Updating" : api.running ? "Stop" : "Start"}</button></div>
            </div></section>
            {apiError ? <StatusText tone="danger">{apiError}</StatusText> : null}
          </> : null}
        </div>
      </section>
    </div>
  );
}

export function AboutView({ bootstrap, updateCheck, onCheckForUpdates }: { bootstrap: BootstrapState; updateCheck: UpdateCheckStatus; onCheckForUpdates: () => void }) {
  return (
    <div className="page about-page">
      <PageHeader title="About soundAr" actions={<UpdateCheckButton status={updateCheck} onCheck={onCheckForUpdates} />} />
      <div className="about-content">
        <section className="about-product" aria-labelledby="about-product-name">
          <img className="about-app-icon" src="/soundar-app-icon.png" alt="" aria-hidden="true" />
          <div>
            <h2 id="about-product-name">soundAr</h2>
            <p>Local voice and music generation for Linux.</p>
          </div>
          <span className="about-version">Version {__APP_VERSION__}</span>
        </section>
        <UpdateCheckFeedback status={updateCheck} />
        <section className="about-block" aria-labelledby="about-application-heading">
          <h2 id="about-application-heading">Application</h2>
          <div className="settings-group">
            <div className="settings-row"><span><strong>Processing</strong><small>Speech and music render through local model workers.</small></span><span className="settings-value">Local only</span></div>
            <div className="settings-row"><span><strong>Desktop application</strong><small>Native Linux shell with the soundAr React interface.</small></span><span className="settings-value">Tauri 2</span></div>
            <div className="settings-row"><span><strong>Release updates</strong><small>New builds are verified against the signed release feed.</small></span><span className="settings-value">Signed</span></div>
            <div className="settings-row"><span><strong>Cloud fallback</strong><small>Generation content is not sent to a hosted inference service.</small></span><span className="settings-value">Off</span></div>
          </div>
        </section>
        <section className="about-block" aria-labelledby="about-runtime-heading">
          <h2 id="about-runtime-heading">This machine</h2>
          <div className="settings-group">
            <div className="settings-row"><span><strong>Graphics processor</strong><small>{bootstrap.system.gpu_name}</small></span><span className="settings-value">{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB</span></div>
            <div className="settings-row"><span><strong>Acceleration</strong><small>CUDA availability reported by the local desktop bridge.</small></span><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "CUDA ready" : "CPU mode"}</StatusText></div>
            <div className="settings-row"><span><strong>Model library</strong><small>Models currently installed and available to soundAr.</small></span><span className="settings-value">{bootstrap.installed.length} installed</span></div>
          </div>
        </section>
        <footer className="about-footer"><span>Open source</span><span>Private by default</span><span>Built for Linux</span></footer>
      </div>
    </div>
  );
}
