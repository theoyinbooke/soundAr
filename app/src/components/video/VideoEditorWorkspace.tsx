import { Captions, Check, LoaderCircle, Redo2, Save, Undo2 } from "lucide-react";
import { useEffect, useId, useState, type KeyboardEvent } from "react";
import type { VideoJob, VideoProject, VideoScene } from "../../types/video";
import { formatVideoClock, selectPreviewArtifact } from "../../lib/videoState";
import { VideoPreviewPlayer } from "./VideoPreviewPlayer";
import { VideoTimeline } from "./VideoTimeline";

export function VideoEditorWorkspace({
  project,
  selectedSceneId,
  playheadMs,
  job,
  working,
  onSelectScene,
  onPlayheadChange,
  onRenderPreview,
  onExport,
  onSaveScene,
  onCancelWorking,
}: {
  project: VideoProject;
  selectedSceneId?: string;
  playheadMs: number;
  job?: VideoJob;
  working: "rendering" | "exporting" | undefined;
  onSelectScene: (sceneId: string) => void;
  onPlayheadChange: (milliseconds: number) => void;
  onRenderPreview: () => void;
  onExport: () => void;
  onSaveScene: (scene: VideoScene) => Promise<void>;
  onCancelWorking: () => void;
}) {
  const selectedScene = project.manifest.scenes.find((scene) => scene.id === selectedSceneId) ?? project.manifest.scenes[0];
  const preview = selectPreviewArtifact(project);
  const [draft, setDraft] = useState<VideoScene | undefined>(selectedScene);
  const [redoDraft, setRedoDraft] = useState<VideoScene>();
  const [saving, setSaving] = useState(false);
  const [activeTab, setActiveTab] = useState<"layout" | "captions" | "audio">("layout");
  const tabId = useId();

  useEffect(() => {
    setDraft(selectedScene);
    setRedoDraft(undefined);
  }, [selectedScene]);

  const dirty = Boolean(draft && selectedScene && JSON.stringify(draft) !== JSON.stringify(selectedScene));

  function updateDraft(next: VideoScene) {
    setDraft(next);
    setRedoDraft(undefined);
  }

  function undoDraft() {
    if (!draft || !selectedScene || !dirty) return;
    setRedoDraft(draft);
    setDraft(selectedScene);
  }

  async function saveDraft() {
    if (!draft || !dirty) return;
    setSaving(true);
    try { await onSaveScene(draft); }
    finally { setSaving(false); }
  }

  function moveTab(event: KeyboardEvent<HTMLButtonElement>) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const tabs = ["layout", "captions", "audio"] as const;
    const current = tabs.indexOf(activeTab);
    const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (current + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    setActiveTab(tabs[next]);
    document.getElementById(`${tabId}-${tabs[next]}-tab`)?.focus();
  }

  const progress = Math.round((job?.progress ?? 0) * 100);
  return (
    <div className="video-editor-workspace">
      <div className="video-editor-toolbar">
        <div><span>Video Studio</span><h2>{project.name}</h2></div>
        <div><button className="video-icon-button" type="button" aria-label="Undo scene changes" disabled={!dirty || saving || Boolean(working)} onClick={undoDraft}><Undo2 aria-hidden="true" size={15} /></button><button className="video-icon-button" type="button" aria-label="Redo scene changes" disabled={!redoDraft || saving || Boolean(working)} onClick={() => { if (redoDraft) { setDraft(redoDraft); setRedoDraft(undefined); } }}><Redo2 aria-hidden="true" size={15} /></button><button className="video-icon-button" type="button" aria-label="Save scene changes" disabled={!dirty || saving || Boolean(working)} onClick={() => void saveDraft()}>{saving ? <LoaderCircle className="video-spin" aria-hidden="true" size={15} /> : <Save aria-hidden="true" size={15} />}</button><button className="video-button is-primary" type="button" disabled={Boolean(working) || dirty || saving} title={dirty ? "Save scene changes before rendering" : undefined} onClick={preview ? onExport : onRenderPreview}>{working ? <LoaderCircle className="video-spin" aria-hidden="true" size={14} /> : null}{working === "rendering" ? "Rendering preview" : working === "exporting" ? "Exporting" : preview ? "Export video" : "Render preview"}</button></div>
      </div>

      <nav className="video-production-steps" aria-label="Video production progress">
        {[["Source", true], ["Analyze", true], ["Review", true], ["Preview", Boolean(preview)], ["Export", false]].map(([label, complete], index) => <span key={String(label)} className={complete ? "is-complete" : label === (preview ? "Export" : "Preview") ? "is-current" : ""}><i>{complete ? <Check aria-hidden="true" size={11} /> : index + 1}</i>{label}</span>)}
      </nav>

      {working ? <div className="video-render-progress" role="status" aria-live="polite"><div><LoaderCircle className="video-spin" aria-hidden="true" size={15} /><span><strong>{job?.title ?? "Preparing render"}</strong><small>{job?.detail ?? "Starting the durable local job…"}</small></span><b>{progress}%</b><button className="video-button is-secondary" type="button" onClick={onCancelWorking}>Cancel</button></div><i><span style={{ width: `${progress}%` }} /></i></div> : null}

      <div className="video-editor-grid">
        <aside className="video-scene-rail" aria-label="Project scenes">
          <header><strong>Scenes</strong><span>{project.manifest.scenes.length} selected · {formatVideoClock(project.duration_ms)}</span></header>
          <div role="group" aria-label="Scene list">{project.manifest.scenes.map((scene) => <button key={scene.id} className={scene.id === selectedScene?.id ? "is-selected" : ""} type="button" onClick={() => onSelectScene(scene.id)}><span>{scene.position}</span><span><strong>{scene.title}</strong><small>Source {formatVideoClock(scene.source_start_ms)} – {formatVideoClock(scene.source_end_ms)}</small><em>{scene.transcript}</em></span></button>)}</div>
          <footer><span>Source</span><strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></footer>
        </aside>

        <VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} artifact={preview} scene={draft} projectDurationMs={project.duration_ms} playheadMs={playheadMs} onPlayheadChange={onPlayheadChange} />

        <aside className="video-scene-inspector" aria-label="Scene inspector">
          <header><strong>{project.manifest.scenes.length} scenes selected</strong><span>Duration {formatVideoClock(project.duration_ms)}</span></header>
          <div className="video-inspector-tabs" role="tablist" aria-label="Scene settings">{(["layout", "captions", "audio"] as const).map((tab) => <button id={`${tabId}-${tab}-tab`} key={tab} role="tab" aria-selected={activeTab === tab} aria-controls={`${tabId}-${tab}-panel`} tabIndex={activeTab === tab ? 0 : -1} type="button" onClick={() => setActiveTab(tab)} onKeyDown={moveTab}>{tab[0].toUpperCase() + tab.slice(1)}</button>)}</div>
          {draft ? <div className="video-inspector-form">
            {activeTab === "layout" ? <fieldset id={`${tabId}-layout-panel`} role="tabpanel" aria-labelledby={`${tabId}-layout-tab`}><legend>Crop</legend><label><span>Aspect ratio</span><select aria-label="Scene aspect ratio" value={draft.layout === "portrait" ? "9:16" : draft.layout === "landscape" ? "16:9" : "1:1"} onChange={(event) => updateDraft({ ...draft, layout: event.target.value === "9:16" ? "portrait" : event.target.value === "16:9" ? "landscape" : "square" })}><option>9:16</option><option>16:9</option><option>1:1</option></select></label><label><span>Crop mode</span><select aria-label="Scene crop mode" value={draft.crop_mode} onChange={(event) => updateDraft({ ...draft, crop_mode: event.target.value as VideoScene["crop_mode"] })}><option value="auto-center">Auto center</option><option value="fit">Fit</option><option value="manual">Manual</option></select></label></fieldset> : null}
            {activeTab === "captions" ? <fieldset id={`${tabId}-captions-panel`} role="tabpanel" aria-labelledby={`${tabId}-captions-tab`}><legend>Captions</legend><label className="video-switch-row"><span>Show captions</span><input aria-label="Show captions" type="checkbox" checked={draft.captions_enabled} onChange={(event) => updateDraft({ ...draft, captions_enabled: event.target.checked })} /></label><label><span>Style</span><select aria-label="Caption style" value={draft.caption_style} onChange={(event) => updateDraft({ ...draft, caption_style: event.target.value as VideoScene["caption_style"] })}><option value="clean-white">Clean white</option><option value="calm">Calm</option><option value="kinetic">Kinetic</option></select></label></fieldset> : null}
            {activeTab === "audio" ? <fieldset id={`${tabId}-audio-panel`} role="tabpanel" aria-labelledby={`${tabId}-audio-tab`}><legend>Audio mix</legend><label><span>Voice <b>{draft.voice_gain_db.toFixed(1)} dB</b></span><input aria-label="Voice gain" type="range" min={-24} max={6} step={1} value={draft.voice_gain_db} onChange={(event) => updateDraft({ ...draft, voice_gain_db: Number(event.target.value) })} /></label><label><span>Music <b>{draft.music_gain_db.toFixed(1)} dB</b></span><input aria-label="Music gain" type="range" min={-30} max={0} step={1} value={draft.music_gain_db} onChange={(event) => updateDraft({ ...draft, music_gain_db: Number(event.target.value) })} /></label></fieldset> : null}
            <div className={`video-inspector-receipt ${dirty ? "is-dirty" : ""}`}><Captions aria-hidden="true" size={14} /><span>{dirty ? "Unsaved scene changes. Save to invalidate only affected preview and export segments." : "This scene matches the saved timeline version."}</span></div>
          </div> : null}
        </aside>
      </div>

      <VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={playheadMs} selectedSceneId={selectedScene?.id} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} />
      <footer className="video-project-status"><span>Project duration <strong>{formatVideoClock(project.duration_ms)}</strong></span><span>Source <strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></span><span>Cache <strong>2.1 GB</strong></span><span>Autosaved <strong>10:24:18</strong></span></footer>
    </div>
  );
}
