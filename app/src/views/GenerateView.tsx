import { Download, FileAudio, FolderOpen, MoreHorizontal, Pause, Play, Save, Sparkles } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { loadGeneratedAudio, pickAudioFile, synthesizeSpeech } from "../lib/bridge";
import type { BootstrapState, HistoryItem, SynthesisResult, VoiceProfile } from "../types";
import { CompactField, Dropdown, MetricStrip, PageHeader, Panel, Segmented, SelectField, StatusText } from "../components/ui";

const idleWaveform = Array.from({ length: 64 }, (_, index) => 0.12 + Math.abs(Math.sin(index * 0.31)) * 0.13);

function formatPlaybackTime(seconds: number) {
  if (!Number.isFinite(seconds)) return "0:00.0";
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds % 60).toFixed(1).padStart(4, "0")}`;
}

export function GenerateView({
  bootstrap,
  voices,
  onGenerated,
}: {
  bootstrap: BootstrapState;
  voices: VoiceProfile[];
  onGenerated: (item: HistoryItem) => void;
}) {
  const ttsModels = useMemo(() => bootstrap.installed.filter((model) => model.task === "tts"), [bootstrap.installed]);
  const [mode, setMode] = useState<"text" | "ssml" | "batch">("text");
  const [text, setText] = useState("The best voices feel present before they sound perfect.\nStart with clarity, then shape pace, warmth, and intent.");
  const [modelId, setModelId] = useState(ttsModels.find((model) => model.engine === "kokoro")?.model_id ?? ttsModels[0]?.model_id ?? "");
  const [voiceId, setVoiceId] = useState(voices[0]?.id ?? "");
  const [speed, setSpeed] = useState(1);
  const [expressiveness, setExpressiveness] = useState("Balanced");
  const [seed, setSeed] = useState(42817);
  const [outputFormat, setOutputFormat] = useState<"wav" | "flac">("wav");
  const [referencePath, setReferencePath] = useState<string>();
  const [result, setResult] = useState<SynthesisResult>();
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string>();
  const [playbackError, setPlaybackError] = useState<string>();
  const [isPlaying, setIsPlaying] = useState(false);
  const [audioUrl, setAudioUrl] = useState<string>();
  const [isAudioLoading, setIsAudioLoading] = useState(false);
  const [playbackTime, setPlaybackTime] = useState(0);
  const [playbackDuration, setPlaybackDuration] = useState(0);
  const audioRef = useRef<HTMLAudioElement>(null);

  useEffect(() => {
    if (!modelId && ttsModels[0]) setModelId(ttsModels[0].model_id);
  }, [modelId, ttsModels]);

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
    loadGeneratedAudio(result.audio_path, outputFormat)
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
  }, [outputFormat, result?.audio_path]);

  useEffect(() => {
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
  }, [audioUrl, isGenerating, isPlaying, modelId, outputFormat, referencePath, seed, speed, text, voiceId]);

  const selectedModel = ttsModels.find((model) => model.model_id === modelId);
  const selectedVoice = voices.find((voice) => voice.id === voiceId) ?? voices[0];
  const estimatedSeconds = Math.max(2, Math.round(text.length / 13));

  async function generate() {
    if (!text.trim() || !modelId || isGenerating) return;
    setIsGenerating(true);
    setError(undefined);
    try {
      const next = await synthesizeSpeech({
        model_id: modelId,
        text: text.trim(),
        speaker: selectedModel?.engine === "kokoro" ? "af_heart" : selectedVoice?.id ?? "default",
        language: "en",
        reference_audio_path: referencePath,
        speed,
        seed,
        output_format: outputFormat,
      });
      setResult(next);
      setPlaybackError(undefined);
      setIsPlaying(false);
      onGenerated({
        ...next,
        title: text.trim().split(/[.!?]/)[0].slice(0, 44) || "Untitled generation",
        voice: selectedVoice?.name ?? "Default voice",
        text: text.trim(),
      });
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setIsGenerating(false);
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

  return (
    <div className="page generate-page">
      <PageHeader title="Generate" subtitle="Create, stream, compare, and export speech from local open-source engines." />

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
            <span className="composer-count">{text.length} characters / about {estimatedSeconds} seconds</span>
          </div>

          <div className="script-box">
            <textarea
              aria-label="Script"
              value={text}
              onChange={(event) => setText(event.target.value)}
              spellCheck="true"
              placeholder={mode === "batch" ? "Add one generation per line..." : "Write what the voice should say..."}
            />
            <span className="keyboard-hint">Ctrl + Enter to generate</span>
          </div>

          <div className="selector-grid">
            <SelectField label="Model" value={modelId} onChange={setModelId} status={selectedModel ? "Ready" : undefined} options={ttsModels.map((model) => ({ value: model.model_id, label: model.model_id }))} />
            <SelectField label="Voice" value={voiceId} onChange={setVoiceId} status={selectedVoice ? `${selectedVoice.sample_seconds || "Preset"}` : undefined} options={voices.map((voice) => ({ value: voice.id, label: `${voice.name} - ${voice.style}` }))} />
          </div>

          <span className="section-label">Generation settings</span>
          <div className="settings-grid">
            <CompactField label="Speed">
              <div className="range-value">
                <input aria-label="Speed" min="0.7" max="1.3" step="0.05" type="range" value={speed} onChange={(event) => setSpeed(Number(event.target.value))} />
                <strong>{speed.toFixed(2)}x</strong>
              </div>
            </CompactField>
            <CompactField label="Expressiveness">
              <Dropdown ariaLabel="Expressiveness" value={expressiveness} onChange={setExpressiveness} options={["Balanced", "Measured", "Expressive"].map((label) => ({ value: label, label }))} />
            </CompactField>
            <CompactField label="Seed">
              <input type="number" value={seed} onChange={(event) => setSeed(Number(event.target.value))} />
            </CompactField>
            <CompactField label="Output">
              <Dropdown ariaLabel="Output format" value={outputFormat} onChange={(value) => setOutputFormat(value as "wav" | "flac")} options={[{ value: "wav", label: "WAV / source rate" }, { value: "flac", label: "FLAC / source rate" }]} />
            </CompactField>
          </div>

          <div className="waveform-panel">
            <div className="waveform-meta">
              <span className="section-label">Preview</span>
              <span>{formatPlaybackTime(playbackTime)} / {formatPlaybackTime(playbackDuration)}</span>
            </div>
            <button className="audio-waveform" type="button" aria-label="Seek audio preview" disabled={!audioUrl} onClick={seekPlayback}>
              {waveform.map((peak, index) => (
                <i className={(index + 1) / waveform.length <= playbackProgress ? "is-played" : ""} key={`${index}-${peak}`} style={{ height: `${Math.max(4, Math.round(peak * 32))}px` }} />
              ))}
            </button>
            {audioUrl ? <audio ref={audioRef} className="visually-hidden" preload="auto" src={audioUrl} onCanPlay={() => setPlaybackError(undefined)} onLoadedMetadata={(event) => setPlaybackDuration(event.currentTarget.duration)} onTimeUpdate={(event) => setPlaybackTime(event.currentTarget.currentTime)} onError={() => setPlaybackError("Generated audio could not be decoded") } onEnded={(event) => { setIsPlaying(false); setPlaybackTime(event.currentTarget.duration); }} /> : null}
            <button className="icon-button preview-play" type="button" title={isPlaying ? "Pause preview" : "Play preview"} disabled={!audioUrl || isAudioLoading} onClick={togglePlayback}>
              {isPlaying ? <Pause aria-hidden="true" fill="currentColor" size={13} /> : <Play aria-hidden="true" fill="currentColor" size={13} />}
            </button>
            {isAudioLoading ? <span className="playback-loading">Loading audio...</span> : null}
            {playbackError ? <span className="playback-error">{playbackError}</span> : null}
          </div>

          <div className="composer-footer">
            <div className="composer-state">
              {error ? <StatusText tone="danger">{error}</StatusText> : null}
              {!error && isGenerating ? <StatusText tone="warning">Loading model and generating locally...</StatusText> : null}
              {!error && !isGenerating ? (
                <StatusText tone="success">{result ? `Ready / RTF ${result.rtf.toFixed(2)}x` : "Ready / first audio depends on engine"}</StatusText>
              ) : null}
            </div>
            <button className="button button-secondary" type="button" onClick={async () => setReferencePath(await pickAudioFile())}>
              <FileAudio aria-hidden="true" size={14} />
              {referencePath ? "Reference loaded" : "Reference voice"}
            </button>
            <button className="button button-secondary" type="button">
              <Save aria-hidden="true" size={14} />
              Save preset
            </button>
            <button className="button button-primary" type="button" onClick={generate} disabled={!text.trim() || !modelId || isGenerating}>
              <Sparkles aria-hidden="true" size={14} />
              {isGenerating ? "Generating..." : "Generate audio"}
            </button>
          </div>
        </Panel>

        <Panel className="runtime-rail" ariaLabel="Runtime and output queue">
          <div className="rail-heading">
            <div>
              <span className="section-label">Runtime</span>
              <strong>{selectedModel?.model_id.split("/").at(-1) ?? "No model"}</strong>
            </div>
            <StatusText tone={selectedModel ? "success" : "warning"}>{selectedModel ? "Worker ready" : "Install a model"}</StatusText>
          </div>

          <MetricStrip
            metrics={[
              { value: result ? `${result.inference_seconds.toFixed(2)} s` : "--", label: "Inference", tone: "success" },
              { value: result ? `${result.rtf.toFixed(2)}x` : "--", label: "RTF", tone: "success" },
              { value: result ? `${(result.vram_peak_mb / 1024).toFixed(1)} GB` : "--", label: "Peak VRAM", tone: "warning" },
            ]}
          />

          <span className="section-label rail-section">Queue</span>
          <div className="queue-list">
            <div className="queue-row">
              <div><strong>{text.split(/[.!?]/)[0].slice(0, 28) || "Current script"}</strong><StatusText tone={isGenerating ? "warning" : result ? "success" : "muted"}>{isGenerating ? "Generating" : result ? "Ready" : "Draft"}</StatusText></div>
              <MoreHorizontal aria-hidden="true" size={16} />
            </div>
            <div className="queue-row">
              <div><strong>Product tagline</strong><StatusText tone="muted">Queued</StatusText></div>
              <MoreHorizontal aria-hidden="true" size={16} />
            </div>
            <div className="queue-row">
              <div><strong>Chapter outro</strong><StatusText tone="muted">Saved</StatusText></div>
              <MoreHorizontal aria-hidden="true" size={16} />
            </div>
          </div>

          <span className="section-label rail-section">Output</span>
          <div className="output-block">
            <strong>{outputFormat.toUpperCase()} / source sample rate / mono</strong>
            <span>{bootstrap.export_dir}</span>
            <button className="text-button" type="button">Change</button>
          </div>

          <div className="rail-actions">
            <button className="button button-secondary" type="button" disabled={!result}>
              <FolderOpen aria-hidden="true" size={14} />
              Open output
            </button>
            <button className="icon-button" type="button" title="Export audio" disabled={!result}>
              <Download aria-hidden="true" size={15} />
            </button>
          </div>
        </Panel>
      </div>
    </div>
  );
}
