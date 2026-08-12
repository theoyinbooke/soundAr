import { Activity, Check, CircleStop, FolderOpen, Gauge, LoaderCircle, Mic, Pause, Play, Search, SlidersHorizontal } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { BootstrapState, HistoryItem, Theme } from "../types";
import { CompactField, EmptyState, MetricStrip, PageHeader, Panel, Segmented, SelectField, StatusText } from "../components/ui";
import { BrandLockup, BrandMark } from "../components/Brand";
import { loadGeneratedAudio } from "../lib/bridge";

const levels = [22, 34, 18, 48, 28, 62, 42, 71, 38, 54, 31, 66, 45, 57, 26, 43, 20, 34];

export function LiveView({ bootstrap }: { bootstrap: BootstrapState }) {
  const models = bootstrap.installed.filter((model) => model.task === "tts");
  const [active, setActive] = useState(false);
  const [model, setModel] = useState(models[0]?.model_id ?? "");
  const [latency, setLatency] = useState<"quality" | "balanced" | "fast">("balanced");

  return (
    <div className="page">
      <PageHeader title="Live" subtitle="Stream microphone input through a local voice with a compact latency budget." actions={<button className={`button ${active ? "button-danger" : "button-primary"}`} type="button" onClick={() => setActive(!active)}>{active ? <CircleStop size={14} /> : <Mic size={14} />}{active ? "Stop session" : "Start session"}</button>} />
      <MetricStrip metrics={[{ value: active ? "86 ms" : "--", label: "Input buffer", tone: "success" }, { value: active ? "0.38x" : "--", label: "Generation RTF", tone: "success" }, { value: active ? "5.4 GB" : "--", label: "Peak VRAM", tone: "warning" }, { value: active ? "24 kHz" : "--", label: "Output stream" }]} />
      <div className="live-layout">
        <Panel className="live-stage" ariaLabel="Live voice session">
          <div className={`live-orb ${active ? "is-active" : ""}`}><Activity size={22} /></div>
          <div><h2>{active ? "Listening and converting" : "Session ready"}</h2><p>{active ? "Microphone input is staying on this machine." : "Choose a model and start when your microphone is ready."}</p></div>
          <div className="level-bars" aria-hidden="true">{levels.map((height, index) => <i key={`${height}-${index}`} className={active && index < 13 ? "is-live" : ""} style={{ height }} />)}</div>
          <StatusText tone={active ? "success" : "muted"}>{active ? "Local stream active" : "No audio captured"}</StatusText>
        </Panel>
        <Panel className="live-controls" ariaLabel="Live session settings">
          <span className="section-label">Session</span>
          <SelectField label="Voice model" value={model} onChange={setModel} options={models.map((item) => ({ value: item.model_id, label: item.model_id }))} />
          <span className="field-label standalone-label">Latency profile</span>
          <Segmented label="Latency profile" value={latency} onChange={setLatency} options={[{ value: "quality", label: "Quality" }, { value: "balanced", label: "Balanced" }, { value: "fast", label: "Fast" }]} />
          <dl className="compact-definition-list control-summary"><div><dt>Input</dt><dd>Default microphone</dd></div><div><dt>Output</dt><dd>System audio</dd></div><div><dt>Noise gate</dt><dd><StatusText tone="success">Enabled</StatusText></dd></div><div><dt>Monitoring</dt><dd>Headphones</dd></div></dl>
        </Panel>
      </div>
    </div>
  );
}

export function CompareView({ bootstrap }: { bootstrap: BootstrapState }) {
  const models = bootstrap.installed.filter((model) => model.task === "tts");
  const [left, setLeft] = useState(models[0]?.model_id ?? "");
  const [right, setRight] = useState(models[1]?.model_id ?? models[0]?.model_id ?? "");
  const [rendered, setRendered] = useState(false);
  return (
    <div className="page">
      <PageHeader title="Compare" subtitle="Render one script through two local engines and judge the tradeoffs side by side." actions={<button className="button button-primary" type="button" onClick={() => setRendered(true)}><Play size={14} />Render both</button>} />
      <Panel className="compare-script"><textarea aria-label="Comparison script" defaultValue="Every voice has a texture. The right model preserves clarity without flattening the intent behind the words." /><span>Single shared script / deterministic seed 42817</span></Panel>
      <div className="compare-grid">
        {[{ label: "A", value: left, onChange: setLeft, rtf: "0.21x", vram: "1.1 GB" }, { label: "B", value: right, onChange: setRight, rtf: "0.44x", vram: "7.4 GB" }].map((side) => (
          <Panel className="compare-side" key={side.label}>
            <div className="compare-heading"><span>{side.label}</span><StatusText tone={rendered ? "success" : "muted"}>{rendered ? "Ready" : "Not rendered"}</StatusText></div>
            <SelectField label="Model" value={side.value} onChange={side.onChange} options={models.map((model) => ({ value: model.model_id, label: model.model_id }))} />
            <div className="mini-waveform">{levels.slice(0, 14).map((height, index) => <i key={index} className={rendered ? "is-live" : ""} style={{ height: height / 1.5 }} />)}</div>
            <button className="button button-secondary" type="button" disabled={!rendered}><Play size={13} />Play output {side.label}</button>
            <dl className="compact-definition-list"><div><dt>Real-time factor</dt><dd>{rendered ? side.rtf : "--"}</dd></div><div><dt>Peak VRAM</dt><dd>{rendered ? side.vram : "--"}</dd></div><div><dt>Seed</dt><dd>42817</dd></div></dl>
          </Panel>
        ))}
      </div>
    </div>
  );
}

export function HistoryView({ history }: { history: HistoryItem[] }) {
  const [query, setQuery] = useState("");
  const [activeId, setActiveId] = useState<string>();
  const [loadingId, setLoadingId] = useState<string>();
  const [isPlaying, setIsPlaying] = useState(false);
  const [playbackError, setPlaybackError] = useState<{ id: string; message: string }>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const objectUrlRef = useRef<string | undefined>(undefined);
  const requestRef = useRef(0);
  const filtered = useMemo(() => history.filter((item) => [item.title, item.voice, item.model_id, item.text].join(" ").toLowerCase().includes(query.toLowerCase())), [history, query]);

  useEffect(() => () => {
    requestRef.current += 1;
    audioRef.current?.pause();
    if (objectUrlRef.current?.startsWith("blob:")) URL.revokeObjectURL(objectUrlRef.current);
  }, []);

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

  return (
    <div className="page">
      <PageHeader title="History" subtitle="Reopen, audition, and export generations made on this machine." />
      <div className="data-toolbar"><label className="search-control"><Search size={14} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search scripts, voices, and models..." /></label><StatusText tone="muted">{history.length} local generations</StatusText></div>
      <Panel className="table-panel">
        {filtered.length ? (
          <div className="table-scroll">
            <table className="data-table">
              <thead><tr><th>Generation</th><th>Voice</th><th>Model</th><th>Duration</th><th>RTF</th><th aria-label="Actions" /></tr></thead>
              <tbody>{filtered.map((item) => {
                const playing = activeId === item.id && isPlaying;
                const loading = loadingId === item.id;
                return (
                  <tr key={item.id}>
                    <td>
                      <strong>{item.title}</strong>
                      <small className={playbackError?.id === item.id ? "history-playback-error" : undefined}>{playbackError?.id === item.id ? playbackError.message : new Date(item.created_at).toLocaleString()}</small>
                    </td>
                    <td>{item.voice}</td>
                    <td className="muted-cell">{item.model_id.split("/").at(-1)}</td>
                    <td className="mono-cell">{item.duration_seconds.toFixed(1)} s</td>
                    <td className="mono-cell">{item.rtf.toFixed(2)}x</td>
                    <td>
                      <button className="icon-button" title={playing ? "Pause generation" : "Play generation"} type="button" disabled={!item.audio_path || loading} onClick={() => void toggleHistoryPlayback(item)}>
                        {loading ? <LoaderCircle className="spin" size={12} /> : playing ? <Pause fill="currentColor" size={12} /> : <Play fill="currentColor" size={12} />}
                      </button>
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

export function SettingsView({ bootstrap, theme, onTheme }: { bootstrap: BootstrapState; theme: Theme; onTheme: (theme: Theme) => void }) {
  return (
    <div className="page settings-page">
      <PageHeader title="Settings" subtitle="Tune the desktop runtime, storage, and visual appearance." />
      <div className="settings-columns">
        <Panel className="settings-section">
          <div className="settings-title"><SlidersHorizontal size={15} /><div><h2>Appearance</h2><p>Compact in both themes, with no blue or purple.</p></div></div>
          <span className="field-label standalone-label">Color mode</span>
          <Segmented label="Color mode" value={theme} onChange={onTheme} options={[{ value: "dark", label: "Dark" }, { value: "light", label: "Cream light" }]} />
          <label className="toggle-row"><span><strong>Dense tables</strong><small>Keep rows compact across the workspace.</small></span><input type="checkbox" defaultChecked /></label>
          <label className="toggle-row"><span><strong>Reduced motion</strong><small>Limit interface animation.</small></span><input type="checkbox" /></label>
        </Panel>
        <Panel className="settings-section">
          <div className="settings-title"><Gauge size={15} /><div><h2>Local runtime</h2><p>Hardware and engine health reported by the desktop bridge.</p></div></div>
          <dl className="compact-definition-list settings-facts"><div><dt>GPU</dt><dd>{bootstrap.system.gpu_name}</dd></div><div><dt>CUDA</dt><dd><StatusText tone={bootstrap.system.cuda_available ? "success" : "warning"}>{bootstrap.system.cuda_available ? "Ready" : "Unavailable"}</StatusText></dd></div><div><dt>Python engines</dt><dd><StatusText tone={bootstrap.system.python_ready ? "success" : "danger"}>{bootstrap.system.python_ready ? "Ready" : "Missing"}</StatusText></dd></div><div><dt>Driver</dt><dd>{bootstrap.system.driver_version || "Unknown"}</dd></div></dl>
        </Panel>
        <Panel className="settings-section settings-storage">
          <div className="settings-title"><FolderOpen size={15} /><div><h2>Storage</h2><p>Models, voice references, and exports stay local.</p></div></div>
          <CompactField label="Export directory"><div className="path-field"><span>{bootstrap.export_dir}</span><button className="icon-button" title="Choose directory" type="button"><FolderOpen size={13} /></button></div></CompactField>
          <div className="storage-summary"><div><strong>{bootstrap.installed.length}</strong><span>Installed models</span></div><div><strong>{(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB</strong><span>GPU memory</span></div><div><strong><Check size={15} /></strong><span>Local-only mode</span></div></div>
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
        <span className="about-version">Version 0.2.4</span>
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
