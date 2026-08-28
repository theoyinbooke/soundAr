import { Captions, Check, LoaderCircle, Redo2, Save, Undo2 } from "lucide-react";
import { useEffect, useId, useState, type KeyboardEvent } from "react";
import { capabilityForModel, compatibleVoicesForModel, qualifiedModels } from "../../lib/capabilities";
import type { BootstrapState, InstalledModel, VoiceProfile } from "../../types";
import type { VideoJob, VideoProject, VideoScene } from "../../types/video";
import { formatVideoClock, formatVideoUpdatedAt, selectPreviewArtifact } from "../../lib/videoState";
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
  bootstrap,
  voices = [],
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
  bootstrap?: BootstrapState;
  voices?: VoiceProfile[];
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
  const narrationModels = bootstrap
    ? qualifiedModels(bootstrap, "tts").filter((model) => (
      model.integrity?.state === "ready"
      && Boolean(model.local_path)
      && compatibleNarrationVoices(bootstrap, model, voices).length > 0
    ))
    : [];
  const narrationModel = narrationModels.find((model) => model.model_id === draft?.model_id);
  const narrationVoices = bootstrap && narrationModel
    ? compatibleNarrationVoices(bootstrap, narrationModel, voices)
    : [];
  const narrationLanguages = bootstrap && narrationModel
    ? supportedNarrationLanguages(bootstrap, narrationModel)
    : [];
  const manualCrop = draft ? cropControls(project, draft) : undefined;

  function selectNarrationModel(modelId: string) {
    if (!bootstrap || !draft) return;
    const model = narrationModels.find((candidate) => candidate.model_id === modelId);
    if (!model) return;
    const compatible = compatibleNarrationVoices(bootstrap, model, voices);
    const voice = compatible.find((candidate) => candidate.id === draft.voice_id) ?? compatible[0];
    const languages = supportedNarrationLanguages(bootstrap, model);
    const language = languages.includes(draft.language ?? "") ? draft.language! : languages[0];
    if (!voice || !language) return;
    updateDraft({
      ...draft,
      model_id: model.model_id,
      voice_id: voice.id,
      speaker: narrationSpeaker(voice),
      language,
    });
  }

  function selectNarrationVoice(voiceId: string) {
    if (!draft || !narrationModel) return;
    const voice = narrationVoices.find((candidate) => candidate.id === voiceId);
    const language = narrationLanguages.includes(draft.language ?? "")
      ? draft.language!
      : narrationLanguages[0];
    if (!voice || !language) return;
    updateDraft({
      ...draft,
      model_id: narrationModel.model_id,
      voice_id: voice.id,
      speaker: narrationSpeaker(voice),
      language,
    });
  }
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
            {activeTab === "layout" ? <fieldset id={`${tabId}-layout-panel`} role="tabpanel" aria-labelledby={`${tabId}-layout-tab`}><legend>Crop</legend><label><span>Aspect ratio</span><select aria-label="Scene aspect ratio" value={draft.layout === "portrait" ? "9:16" : draft.layout === "landscape" ? "16:9" : "1:1"} onChange={(event) => {
              const layout = event.target.value === "9:16" ? "portrait" : event.target.value === "16:9" ? "landscape" : "square";
              updateDraft({ ...draft, layout, crop_rect: draft.crop_mode === "manual" ? manualCropRect(project, layout, 50, 50, manualCrop?.zoom ?? 1) : undefined });
            }}><option>9:16</option><option>16:9</option><option>1:1</option></select></label><label><span>Crop mode</span><select aria-label="Scene crop mode" value={draft.crop_mode} onChange={(event) => {
              const cropMode = event.target.value as VideoScene["crop_mode"];
              updateDraft({ ...draft, crop_mode: cropMode, crop_rect: cropMode === "manual" ? manualCropRect(project, draft.layout, 50, 50, 1) : undefined });
            }}><option value="auto-center">Auto center</option><option value="fit">Fit</option><option value="manual">Manual</option></select></label>{draft.crop_mode === "manual" && manualCrop ? <>
              <label><span>Focus X <b>{Math.round(manualCrop.focusX)}%</b></span><input aria-label="Manual crop focus X" type="range" min={0} max={100} step={1} value={manualCrop.focusX} onChange={(event) => updateDraft({ ...draft, crop_rect: manualCropRect(project, draft.layout, Number(event.target.value), manualCrop.focusY, manualCrop.zoom) })} /></label>
              <label><span>Focus Y <b>{Math.round(manualCrop.focusY)}%</b></span><input aria-label="Manual crop focus Y" type="range" min={0} max={100} step={1} value={manualCrop.focusY} onChange={(event) => updateDraft({ ...draft, crop_rect: manualCropRect(project, draft.layout, manualCrop.focusX, Number(event.target.value), manualCrop.zoom) })} /></label>
              <label><span>Zoom <b>{manualCrop.zoom.toFixed(1)}×</b></span><input aria-label="Manual crop zoom" type="range" min={1} max={3} step={0.1} value={manualCrop.zoom} onChange={(event) => updateDraft({ ...draft, crop_rect: manualCropRect(project, draft.layout, manualCrop.focusX, manualCrop.focusY, Number(event.target.value)) })} /></label>
            </> : null}</fieldset> : null}
            {activeTab === "captions" ? <fieldset id={`${tabId}-captions-panel`} role="tabpanel" aria-labelledby={`${tabId}-captions-tab`}><legend>Captions</legend><label className="video-switch-row"><span>Show captions</span><input aria-label="Show captions" type="checkbox" checked={draft.captions_enabled} onChange={(event) => updateDraft({ ...draft, captions_enabled: event.target.checked })} /></label><label><span>Style</span><select aria-label="Caption style" value={draft.caption_style} onChange={(event) => updateDraft({ ...draft, caption_style: event.target.value as VideoScene["caption_style"] })}><option value="clean-white">Clean white</option><option value="calm">Calm</option><option value="kinetic">Kinetic</option></select></label></fieldset> : null}
            {activeTab === "audio" ? <div id={`${tabId}-audio-panel`} role="tabpanel" aria-labelledby={`${tabId}-audio-tab`}>
              <fieldset><legend>Narration route</legend>
                {narrationModels.length ? <>
                  <label><span>Model</span><select aria-label="Narration model" value={narrationModel?.model_id ?? ""} onChange={(event) => selectNarrationModel(event.target.value)}><option value="" disabled>Choose model</option>{narrationModels.map((model) => <option key={model.model_id} value={model.model_id}>{model.model_id}</option>)}</select></label>
                  <label><span>Voice</span><select aria-label="Narration voice" value={narrationVoices.some((voice) => voice.id === draft.voice_id) ? draft.voice_id : ""} disabled={!narrationModel} onChange={(event) => selectNarrationVoice(event.target.value)}><option value="" disabled>Choose voice</option>{narrationVoices.map((voice) => <option key={voice.id} value={voice.id}>{voice.name}</option>)}</select></label>
                  <label><span>Language</span><select aria-label="Narration language" value={narrationLanguages.includes(draft.language ?? "") ? draft.language : ""} disabled={!narrationModel} onChange={(event) => {
                    const voice = narrationVoices.find((candidate) => candidate.id === draft.voice_id);
                    if (!narrationModel || !voice) return;
                    updateDraft({ ...draft, model_id: narrationModel.model_id, voice_id: voice.id, speaker: narrationSpeaker(voice), language: event.target.value });
                  }}><option value="" disabled>Choose language</option>{narrationLanguages.map((language) => <option key={language} value={language}>{language}</option>)}</select></label>
                </> : <p className="video-route-empty">Install a ready speech model and add a compatible consent-cleared voice to change narration.</p>}
              </fieldset>
              <fieldset><legend>Audio mix</legend><label><span>Voice <b>{draft.voice_gain_db.toFixed(1)} dB</b></span><input aria-label="Voice gain" type="range" min={-24} max={6} step={1} value={draft.voice_gain_db} onChange={(event) => updateDraft({ ...draft, voice_gain_db: Number(event.target.value) })} /></label><label><span>Music <b>{draft.music_gain_db.toFixed(1)} dB</b></span><input aria-label="Music gain" type="range" min={-30} max={0} step={1} value={draft.music_gain_db} onChange={(event) => updateDraft({ ...draft, music_gain_db: Number(event.target.value) })} /></label></fieldset>
            </div> : null}
            <div className={`video-inspector-receipt ${dirty ? "is-dirty" : ""}`}><Captions aria-hidden="true" size={14} /><span>{dirty ? "Unsaved scene changes. Save to invalidate only affected preview and export segments." : "This scene matches the saved timeline version."}</span></div>
          </div> : null}
        </aside>
      </div>

      <VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={playheadMs} selectedSceneId={selectedScene?.id} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} />
      <footer className="video-project-status"><span>Project duration <strong>{formatVideoClock(project.duration_ms)}</strong></span><span>Source <strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></span><span>Revision <strong>{project.revision}</strong></span><span>Saved <strong>{formatVideoUpdatedAt(project.updated_at)}</strong></span></footer>
    </div>
  );
}

function compatibleNarrationVoices(
  bootstrap: BootstrapState,
  model: InstalledModel,
  voices: VoiceProfile[],
): VoiceProfile[] {
  return compatibleVoicesForModel(bootstrap, model, voices).filter((voice) => (
    voice.state === "preset"
      ? voice.consent === "not-required"
      : voice.state === "ready" && voice.consent === "confirmed" && Boolean(voice.local_path)
  ));
}

function supportedNarrationLanguages(bootstrap: BootstrapState, model: InstalledModel): string[] {
  const declared = capabilityForModel(bootstrap, model)?.languages ?? [];
  const installed = model.languages.length ? model.languages : declared;
  return [...new Set(installed.filter((language) => declared.length === 0 || declared.includes(language)))];
}

function narrationSpeaker(voice: VoiceProfile): string {
  return voice.state === "preset" || voice.source_kind === "preset" ? voice.id : "default";
}

function baseCropSize(project: VideoProject, layout: VideoScene["layout"]): { width: number; height: number } {
  const sourceWidth = project.manifest.source.width ?? 16;
  const sourceHeight = project.manifest.source.height ?? 9;
  const sourceAspect = sourceWidth / sourceHeight;
  const targetAspect = layout === "portrait" ? 9 / 16 : layout === "landscape" ? 16 / 9 : 1;
  return sourceAspect > targetAspect
    ? { width: 10_000 * targetAspect / sourceAspect, height: 10_000 }
    : { width: 10_000, height: 10_000 * sourceAspect / targetAspect };
}

function manualCropRect(
  project: VideoProject,
  layout: VideoScene["layout"],
  focusX: number,
  focusY: number,
  zoom: number,
): NonNullable<VideoScene["crop_rect"]> {
  const base = baseCropSize(project, layout);
  const safeZoom = Math.max(1, Math.min(3, zoom));
  const width = Math.max(1, Math.round(base.width / safeZoom));
  const height = Math.max(1, Math.round(base.height / safeZoom));
  const x = Math.round(Math.max(0, Math.min(10_000 - width, focusX * 100 - width / 2)));
  const y = Math.round(Math.max(0, Math.min(10_000 - height, focusY * 100 - height / 2)));
  return { x_bp: x, y_bp: y, width_bp: width, height_bp: height };
}

function cropControls(project: VideoProject, scene: VideoScene): { focusX: number; focusY: number; zoom: number } {
  const base = baseCropSize(project, scene.layout);
  const rect = scene.crop_rect ?? manualCropRect(project, scene.layout, 50, 50, 1);
  return {
    focusX: Math.max(0, Math.min(100, (rect.x_bp + rect.width_bp / 2) / 100)),
    focusY: Math.max(0, Math.min(100, (rect.y_bp + rect.height_bp / 2) / 100)),
    zoom: Math.max(1, Math.min(3, Math.min(base.width / rect.width_bp, base.height / rect.height_bp))),
  };
}
