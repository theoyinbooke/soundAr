import { Captions, ImagePlus, LoaderCircle, Plus, Redo2, Save, Undo2 } from "lucide-react";
import { useEffect, useId, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { capabilityForModel, compatibleVoicesForModel, qualifiedModels } from "../../lib/capabilities";
import type { BootstrapState, InstalledModel, VoiceProfile } from "../../types";
import type { VideoCanvasBounds, VideoCaptionStyle, VideoJob, VideoProject, VideoScene, VideoTimelineOperation, VideoVisualLayer } from "../../types/video";
import { formatVideoClock, formatVideoUpdatedAt, selectPreviewArtifact } from "../../lib/videoState";
import { VideoPreviewPlayer } from "./VideoPreviewPlayer";
import { VideoProductionSteps } from "./VideoProductionSteps";
import { millisecondsToMicroseconds, VideoTimeline, type VideoTimelineMode } from "./VideoTimeline";

type VideoInspectorTab = "layout" | "captions" | "audio" | "visuals";

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
  onAddVisual,
  visualAdding,
  onEditTimeline,
  timelineEditing,
  timelineFeedback,
  canUndoTimeline,
  canRedoTimeline,
  onUndoTimeline,
  onRedoTimeline,
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
  onAddVisual: (sceneId?: string) => Promise<boolean>;
  visualAdding: boolean;
  onEditTimeline: (operations: VideoTimelineOperation[], label: string) => Promise<void>;
  timelineEditing: boolean;
  timelineFeedback?: { tone: "status" | "error"; text: string };
  canUndoTimeline: boolean;
  canRedoTimeline: boolean;
  onUndoTimeline: () => void;
  onRedoTimeline: () => void;
  onCancelWorking: () => void;
  bootstrap?: BootstrapState;
  voices?: VoiceProfile[];
}) {
  const selectedScene = project.manifest.scenes.find((scene) => scene.id === selectedSceneId) ?? project.manifest.scenes[0];
  const preview = selectPreviewArtifact(project);
  const [draft, setDraft] = useState<VideoScene | undefined>(selectedScene);
  const [redoDraft, setRedoDraft] = useState<VideoScene>();
  const [saving, setSaving] = useState(false);
  const [selectedVisualLayerId, setSelectedVisualLayerId] = useState<string>();
  const visualLayers = project.manifest.visual_layers ?? [];
  const fallbackVisualLayer = [...visualLayers].reverse().find((layer) => (
    layer.scene_id === selectedScene?.id && playheadMs >= layer.start_ms && playheadMs <= layer.end_ms
  )) ?? [...visualLayers].reverse().find((layer) => layer.scene_id === selectedScene?.id)
    ?? [...visualLayers].reverse().find((layer) => !layer.scene_id && playheadMs >= layer.start_ms && playheadMs <= layer.end_ms);
  const selectedVisualLayer = visualLayers.find((layer) => layer.id === selectedVisualLayerId && (
    layer.scene_id === selectedScene?.id || (!layer.scene_id && playheadMs >= layer.start_ms && playheadMs <= layer.end_ms)
  ));
  const activeVisualLayer = selectedVisualLayer ?? fallbackVisualLayer;
  const activeVisualAsset = (project.manifest.visual_assets ?? []).find((asset) => asset.id === activeVisualLayer?.asset_id);
  const inspectorTabs: readonly VideoInspectorTab[] = activeVisualLayer
    ? ["layout", "captions", "audio", "visuals"]
    : ["layout", "captions", "audio"];
  const [activeTab, setActiveTab] = useState<VideoInspectorTab>("captions");
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [scenePaneWidth, setScenePaneWidth] = useState(190);
  const [inspectorPaneWidth, setInspectorPaneWidth] = useState(270);
  const [timelineMode, setTimelineMode] = useState<VideoTimelineMode>(() => readTimelineMode());
  const [timelineHeight, setTimelineHeight] = useState(() => timelineHeightForMode(readTimelineMode()));
  const tabId = useId();
  const workspaceRef = useRef<HTMLDivElement>(null);
  const compactPaneLayout = useRef(false);

  useEffect(() => {
    setDraft(selectedScene);
    setRedoDraft(undefined);
    setSelectedVisualLayerId(undefined);
  }, [selectedScene]);

  useEffect(() => {
    if (activeTab === "visuals" && !activeVisualLayer) setActiveTab("captions");
  }, [activeTab, activeVisualLayer]);

  useEffect(() => {
    try { window.localStorage.setItem(TIMELINE_MODE_KEY, timelineMode); } catch { /* Preference storage is optional. */ }
  }, [timelineMode]);

  useEffect(() => {
    if (typeof ResizeObserver === "undefined" || !workspaceRef.current) return;
    const observer = new ResizeObserver(([entry]) => {
      const compact = entry.contentRect.width <= 900;
      if (compact === compactPaneLayout.current) return;
      compactPaneLayout.current = compact;
      if (compact) {
        setScenePaneWidth((width) => Math.min(width, 160));
        setInspectorPaneWidth((width) => Math.min(width, 230));
      } else {
        setScenePaneWidth((width) => width === 160 ? 190 : width);
        setInspectorPaneWidth((width) => width === 230 ? 270 : width);
      }
    });
    observer.observe(workspaceRef.current);
    return () => observer.disconnect();
  }, []);

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
    const focused = inspectorTabs.findIndex((tab) => event.currentTarget.id === `${tabId}-${tab}-tab`);
    const current = focused >= 0 ? focused : inspectorTabs.indexOf(activeTab);
    const next = event.key === "Home" ? 0 : event.key === "End" ? inspectorTabs.length - 1 : (current + (event.key === "ArrowRight" ? 1 : -1) + inspectorTabs.length) % inspectorTabs.length;
    setActiveTab(inspectorTabs[next]);
    document.getElementById(`${tabId}-${inspectorTabs[next]}-tab`)?.focus();
  }

  function beginPaneResize(kind: "scenes" | "inspector", event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const startValue = kind === "scenes" ? scenePaneWidth : inspectorPaneWidth;
    const move = (pointer: PointerEvent) => {
      const delta = pointer.clientX - startX;
      if (kind === "scenes") setScenePaneWidth(clamp(startValue + delta, 150, 300));
      else setInspectorPaneWidth(clamp(startValue - delta, 230, 380));
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  function resizePaneWithKeyboard(kind: "scenes" | "inspector", event: KeyboardEvent<HTMLDivElement>) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    if (kind === "scenes") {
      const next = event.key === "Home" ? 150 : event.key === "End" ? 300 : scenePaneWidth + (event.key === "ArrowRight" ? 12 : -12);
      setScenePaneWidth(clamp(next, 150, 300));
    } else {
      const next = event.key === "Home" ? 230 : event.key === "End" ? 380 : inspectorPaneWidth + (event.key === "ArrowLeft" ? 12 : -12);
      setInspectorPaneWidth(clamp(next, 230, 380));
    }
  }

  function changeTimelineMode(mode: VideoTimelineMode) {
    setTimelineMode(mode);
    setTimelineHeight(timelineHeightForMode(mode));
  }

  function resizeTimeline(nextHeight: number) {
    if (nextHeight <= 120) {
      setTimelineMode("collapsed");
      setTimelineHeight(48);
    } else if (nextHeight >= 300) {
      setTimelineMode("expanded");
      setTimelineHeight(Math.max(320, nextHeight));
    } else {
      setTimelineMode("compact");
      setTimelineHeight(Math.max(210, nextHeight));
    }
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
  const captionBounds = draft?.caption_bounds ?? DEFAULT_CAPTION_BOUNDS;

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

  function commitVisualLayer(next: VideoVisualLayer, label: string) {
    if (timelineEditing) return;
    void onEditTimeline([visualLayerOperation(next)], label);
  }

  function commitVisualBounds(layerId: string, bounds: VideoCanvasBounds) {
    const layer = visualLayers.find((candidate) => candidate.id === layerId);
    if (!layer) return;
    commitVisualLayer({
      ...layer,
      motion: { ...layer.motion, start_bounds: bounds, end_bounds: bounds },
    }, "Place image");
  }
  return (
    <div className="video-editor-workspace" ref={workspaceRef}>
      <div className="video-editor-toolbar">
        <div><span>Video Studio</span><h2 title={project.name}>{project.name}</h2></div>
        <div><div className="video-add-control"><button className="video-button is-secondary" type="button" aria-haspopup="menu" aria-expanded={addMenuOpen} disabled={visualAdding || timelineEditing || Boolean(working)} onClick={() => setAddMenuOpen((open) => !open)}>{visualAdding ? <LoaderCircle className="video-spin" aria-hidden="true" size={14} /> : <Plus aria-hidden="true" size={14} />}{visualAdding ? "Adding" : "Add"}</button>{addMenuOpen ? <div className="video-add-menu" role="menu" aria-label="Add element"><button role="menuitem" type="button" onClick={() => { if (draft && !draft.captions_enabled) updateDraft({ ...draft, captions_enabled: true }); setActiveTab("captions"); setAddMenuOpen(false); }}><Captions aria-hidden="true" size={14} /><span><strong>{draft?.captions_enabled ? "Edit captions" : "Add captions"}</strong><small>Use source-clock transcript cues</small></span></button><button role="menuitem" type="button" onClick={() => { setAddMenuOpen(false); void onAddVisual(draft?.id).then((added) => { if (added) setActiveTab("visuals"); }); }}><ImagePlus aria-hidden="true" size={14} /><span><strong>Add image</strong><small>Choose a PNG, JPEG, or WebP</small></span></button></div> : null}</div><button className="video-icon-button" type="button" aria-label={dirty ? "Undo unsaved scene changes" : "Undo last timeline edit"} disabled={(!dirty && !canUndoTimeline) || saving || timelineEditing || visualAdding || Boolean(working)} onClick={dirty ? undoDraft : onUndoTimeline}><Undo2 aria-hidden="true" size={15} /></button><button className="video-icon-button" type="button" aria-label={redoDraft ? "Redo unsaved scene changes" : "Redo last timeline edit"} disabled={(!redoDraft && !canRedoTimeline) || saving || timelineEditing || visualAdding || Boolean(working)} onClick={redoDraft ? () => { setDraft(redoDraft); setRedoDraft(undefined); } : onRedoTimeline}><Redo2 aria-hidden="true" size={15} /></button><button className="video-icon-button" type="button" aria-label="Save scene changes" disabled={!dirty || saving || timelineEditing || visualAdding || Boolean(working)} onClick={() => void saveDraft()}>{saving ? <LoaderCircle className="video-spin" aria-hidden="true" size={15} /> : <Save aria-hidden="true" size={15} />}</button><button className="video-button is-primary" type="button" disabled={Boolean(working) || dirty || saving || timelineEditing || visualAdding} title={dirty ? "Save scene changes before rendering" : undefined} onClick={preview ? onExport : onRenderPreview}>{working ? <LoaderCircle className="video-spin" aria-hidden="true" size={14} /> : null}{working === "rendering" ? "Rendering preview" : working === "exporting" ? "Exporting" : preview ? "Export video" : "Render preview"}</button></div>
      </div>

      <VideoProductionSteps project={project} />

      {working ? <div className="video-render-progress" role="status" aria-live="polite"><div><LoaderCircle className="video-spin" aria-hidden="true" size={15} /><span><strong>{job?.title ?? "Preparing render"}</strong><small>{job?.detail ?? "Starting the durable local job…"}</small></span><b>{progress}%</b><button className="video-button is-secondary" type="button" onClick={onCancelWorking}>Cancel</button></div><i><span style={{ width: `${progress}%` }} /></i></div> : null}
      {timelineFeedback ? <div className={`video-timeline-feedback is-${timelineFeedback.tone}`} role={timelineFeedback.tone === "error" ? "alert" : "status"}>{timelineFeedback.text}</div> : null}

      <div className="video-editor-grid" data-workspace-layout="resizable" style={{ "--video-scene-pane": `${scenePaneWidth}px`, "--video-inspector-pane": `${inspectorPaneWidth}px`, "--video-timeline-recovery": `${210 - timelineHeight}px` } as CSSProperties}>
        <aside className="video-scene-rail" aria-label="Project scenes">
          <header><strong>Scenes</strong><span>{project.manifest.scenes.length} selected · {formatVideoClock(project.duration_ms)}</span></header>
          <div role="group" aria-label="Scene list">{project.manifest.scenes.map((scene) => { const detail = `${scene.position}. ${scene.title}. Source ${formatVideoClock(scene.source_start_ms)} to ${formatVideoClock(scene.source_end_ms)}. ${scene.transcript}`; return <button key={scene.id} className={scene.id === selectedScene?.id ? "is-selected" : ""} type="button" aria-label={detail} title={detail} onClick={() => onSelectScene(scene.id)}><span>{scene.position}</span><strong>{scene.title}</strong></button>; })}</div>
          <footer><span>Source</span><strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></footer>
        </aside>

        <div className="video-pane-resizer is-scenes" role="separator" aria-label="Resize scenes panel" aria-orientation="vertical" aria-valuemin={150} aria-valuemax={300} aria-valuenow={scenePaneWidth} tabIndex={0} onPointerDown={(event) => beginPaneResize("scenes", event)} onKeyDown={(event) => resizePaneWithKeyboard("scenes", event)} />

        <VideoPreviewPlayer sourceUrl={project.manifest.source.preview_url} artifact={preview} scene={draft} scenes={project.manifest.scenes} transcript={project.manifest.transcript} captionPages={project.manifest.caption_pages} visualAssets={project.manifest.visual_assets} visualLayers={project.manifest.visual_layers} projectDurationMs={project.duration_ms} playheadMs={playheadMs} onPlayheadChange={onPlayheadChange} onSelectCaption={() => setActiveTab("captions")} onCaptionBoundsChange={(caption_bounds) => { if (draft) updateDraft({ ...draft, caption_bounds }); }} selectedVisualLayerId={activeTab === "visuals" ? activeVisualLayer?.id : selectedVisualLayerId} onSelectVisual={(layerId) => { setSelectedVisualLayerId(layerId); setActiveTab("visuals"); }} onVisualBoundsChange={timelineEditing ? undefined : commitVisualBounds} />

        <div className="video-pane-resizer is-inspector" role="separator" aria-label="Resize scene inspector" aria-orientation="vertical" aria-valuemin={230} aria-valuemax={380} aria-valuenow={inspectorPaneWidth} tabIndex={0} onPointerDown={(event) => beginPaneResize("inspector", event)} onKeyDown={(event) => resizePaneWithKeyboard("inspector", event)} />

        <aside className="video-scene-inspector" aria-label="Scene inspector">
          <header><strong>{project.manifest.scenes.length} scenes selected</strong><span>Duration {formatVideoClock(project.duration_ms)}</span></header>
          <div className="video-inspector-tabs" role="tablist" aria-label="Scene settings">{inspectorTabs.map((tab) => <button id={`${tabId}-${tab}-tab`} key={tab} role="tab" aria-selected={activeTab === tab} aria-controls={`${tabId}-${tab}-panel`} tabIndex={activeTab === tab ? 0 : -1} type="button" onClick={() => setActiveTab(tab)} onKeyDown={moveTab}>{tab === "visuals" ? "Visual" : tab[0].toUpperCase() + tab.slice(1)}</button>)}</div>
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
            {activeTab === "captions" ? <fieldset id={`${tabId}-captions-panel`} role="tabpanel" aria-labelledby={`${tabId}-captions-tab`}><legend>Captions</legend><label className="video-switch-row"><span>Show captions</span><input aria-label="Show captions" type="checkbox" checked={draft.captions_enabled} onChange={(event) => updateDraft({ ...draft, captions_enabled: event.target.checked })} /></label><div className="video-caption-presets" role="radiogroup" aria-label="Caption style">{captionStyles.map((style) => <button key={style.id} className={`video-caption-preset is-${style.id}`} role="radio" aria-checked={draft.caption_style === style.id} type="button" onClick={() => updateDraft({ ...draft, caption_style: style.id })}><span>{style.sample}</span><strong>{style.label}</strong></button>)}</div><div className="video-caption-geometry"><strong>Position &amp; size</strong><small>Drag on canvas or use exact controls.</small><label><span>Horizontal <b>{Math.round(captionBounds.x_bp / 100)}%</b></span><input aria-label="Caption horizontal position" type="range" min={0} max={Math.max(0, 10_000 - captionBounds.width_bp)} step={100} value={captionBounds.x_bp} onChange={(event) => updateDraft({ ...draft, caption_bounds: normalizeCaptionBounds({ ...captionBounds, x_bp: Number(event.target.value) }) })} /></label><label><span>Vertical <b>{Math.round(captionBounds.y_bp / 100)}%</b></span><input aria-label="Caption vertical position" type="range" min={0} max={Math.max(0, 10_000 - captionBounds.height_bp)} step={100} value={captionBounds.y_bp} onChange={(event) => updateDraft({ ...draft, caption_bounds: normalizeCaptionBounds({ ...captionBounds, y_bp: Number(event.target.value) }) })} /></label><label><span>Width <b>{Math.round(captionBounds.width_bp / 100)}%</b></span><input aria-label="Caption width" type="range" min={1600} max={10_000 - captionBounds.x_bp} step={100} value={captionBounds.width_bp} onChange={(event) => updateDraft({ ...draft, caption_bounds: normalizeCaptionBounds({ ...captionBounds, width_bp: Number(event.target.value) }) })} /></label><label><span>Height <b>{Math.round(captionBounds.height_bp / 100)}%</b></span><input aria-label="Caption height" type="range" min={600} max={10_000 - captionBounds.y_bp} step={100} value={captionBounds.height_bp} onChange={(event) => updateDraft({ ...draft, caption_bounds: normalizeCaptionBounds({ ...captionBounds, height_bp: Number(event.target.value) }) })} /></label></div></fieldset> : null}
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
            {activeTab === "visuals" && activeVisualLayer && activeVisualAsset ? <fieldset id={`${tabId}-visuals-panel`} role="tabpanel" aria-labelledby={`${tabId}-visuals-tab`} className="video-visual-inspector">
              <legend>Image layer</legend>
              <div className="video-visual-inspector-card" aria-label="Selected image layer">{activeVisualAsset.url ? <img src={activeVisualAsset.url} alt="" /> : <ImagePlus aria-hidden="true" size={18} />}<span><strong>Imported image</strong><small title={activeVisualAsset.local_path}>{activeVisualAsset.mime_type.replace("image/", "").toUpperCase()} · {activeVisualAsset.width}×{activeVisualAsset.height}</small></span></div>
              <label><span>Fit</span><select aria-label="Image fit" value={activeVisualLayer.fit} disabled={timelineEditing} onChange={(event) => commitVisualLayer({ ...activeVisualLayer, fit: event.target.value as VideoVisualLayer["fit"] }, "Change image fit")}><option value="contain">Contain</option><option value="cover">Cover</option><option value="stretch">Stretch</option></select></label>
              <label><span>Fade in</span><select aria-label="Image fade in" value={activeVisualLayer.transition_in_ms} disabled={timelineEditing} onChange={(event) => commitVisualLayer({ ...activeVisualLayer, transition_in_ms: Number(event.target.value) }, "Change image fade in")}>{visualTransitionOptions(activeVisualLayer, "in").map((milliseconds) => <option key={milliseconds} value={milliseconds}>{milliseconds ? `${milliseconds / 1_000}s` : "None"}</option>)}</select></label>
              <label><span>Fade out</span><select aria-label="Image fade out" value={activeVisualLayer.transition_out_ms} disabled={timelineEditing} onChange={(event) => commitVisualLayer({ ...activeVisualLayer, transition_out_ms: Number(event.target.value) }, "Change image fade out")}>{visualTransitionOptions(activeVisualLayer, "out").map((milliseconds) => <option key={milliseconds} value={milliseconds}>{milliseconds ? `${milliseconds / 1_000}s` : "None"}</option>)}</select></label>
              <dl><div><dt>Range</dt><dd>{formatVideoClock(activeVisualLayer.start_ms)}–{formatVideoClock(activeVisualLayer.end_ms)}</dd></div><div><dt>Placement</dt><dd>{activeVisualLayer.scene_id ? `Scene ${draft.position}` : "Full project"}</dd></div><div><dt>Canvas</dt><dd>{Math.round(activeVisualLayer.motion.start_bounds.width_bp / 100)}% × {Math.round(activeVisualLayer.motion.start_bounds.height_bp / 100)}%</dd></div><div><dt>Motion</dt><dd>{JSON.stringify(activeVisualLayer.motion.start_bounds) === JSON.stringify(activeVisualLayer.motion.end_bounds) ? "Static" : "Pan & zoom"}</dd></div></dl>
              <p>Drag or resize the image on the canvas. Each gesture publishes one revision-bound timeline edit.</p>
            </fieldset> : null}
            <div className={`video-inspector-receipt ${dirty ? "is-dirty" : ""}`}>{activeTab === "visuals" ? <ImagePlus aria-hidden="true" size={14} /> : <Captions aria-hidden="true" size={14} />}<span>{dirty ? "Unsaved scene changes. Save to invalidate only affected preview and export segments." : activeTab === "visuals" ? "This image is published to the current project version." : "This scene matches the saved timeline version."}</span></div>
          </div> : null}
        </aside>
      </div>

      <VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={playheadMs} selectedSceneId={selectedScene?.id} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} onEditTimeline={onEditTimeline} editing={timelineEditing} height={timelineHeight} onHeightChange={resizeTimeline} mode={timelineMode} onModeChange={changeTimelineMode} />
      <footer className="video-project-status"><span>Project duration <strong>{formatVideoClock(project.duration_ms)}</strong></span><span>Source <strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></span><span>Revision <strong>{project.revision}</strong></span><span>Saved <strong>{formatVideoUpdatedAt(project.updated_at)}</strong></span></footer>
    </div>
  );
}

const TIMELINE_MODE_KEY = "soundar.video-studio.timeline-mode";
const DEFAULT_CAPTION_BOUNDS = { x_bp: 800, y_bp: 7350, width_bp: 8400, height_bp: 1500 };

function normalizeCaptionBounds(bounds: typeof DEFAULT_CAPTION_BOUNDS): typeof DEFAULT_CAPTION_BOUNDS {
  const width = Math.max(1_600, Math.min(10_000, Math.round(bounds.width_bp)));
  const height = Math.max(600, Math.min(10_000, Math.round(bounds.height_bp)));
  return {
    x_bp: Math.max(0, Math.min(10_000 - width, Math.round(bounds.x_bp))),
    y_bp: Math.max(0, Math.min(10_000 - height, Math.round(bounds.y_bp))),
    width_bp: width,
    height_bp: height,
  };
}

function readTimelineMode(): VideoTimelineMode {
  try {
    const value = window.localStorage.getItem(TIMELINE_MODE_KEY);
    if (value === "collapsed" || value === "expanded") return value;
  } catch { /* Preference storage is optional. */ }
  return "compact";
}

function timelineHeightForMode(mode: VideoTimelineMode): number {
  return mode === "collapsed" ? 48 : mode === "expanded" ? 360 : 210;
}

function visualLayerOperation(layer: VideoVisualLayer): Extract<VideoTimelineOperation, { type: "update_visual_layer" }> {
  return {
    type: "update_visual_layer",
    layer_id: layer.id,
    scene_id: layer.scene_id ?? null,
    range: { start_us: millisecondsToMicroseconds(layer.start_ms), end_us: millisecondsToMicroseconds(layer.end_ms) },
    fit: layer.fit,
    crop: layer.crop ?? null,
    z_index: layer.z_index,
    motion: layer.motion,
    transition_in_us: millisecondsToMicroseconds(layer.transition_in_ms),
    transition_out_us: millisecondsToMicroseconds(layer.transition_out_ms),
  };
}

function visualTransitionOptions(layer: VideoVisualLayer, edge: "in" | "out"): number[] {
  const duration = Math.max(0, layer.end_ms - layer.start_ms);
  const other = edge === "in" ? layer.transition_out_ms : layer.transition_in_ms;
  const current = edge === "in" ? layer.transition_in_ms : layer.transition_out_ms;
  return [...new Set([0, 150, 300, 600, current])].filter((value) => value >= 0 && value + other <= duration).sort((left, right) => left - right);
}

const captionStyles: Array<{ id: VideoCaptionStyle; label: string; sample: string }> = [
  { id: "clean-white", label: "Clean", sample: "Aa" },
  { id: "calm", label: "Calm", sample: "Aa" },
  { id: "kinetic", label: "Kinetic", sample: "AA" },
  { id: "bold-pop", label: "Bold pop", sample: "POP" },
  { id: "highlight", label: "Highlight", sample: "Mark" },
  { id: "karaoke", label: "Karaoke", sample: "Sing" },
  { id: "typewriter", label: "Typewriter", sample: "Type" },
  { id: "podcast", label: "Podcast", sample: "Talk" },
];

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

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.max(minimum, Math.min(maximum, value));
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
