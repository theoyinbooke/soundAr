import { ChevronDown, ChevronUp, CircleStop, Download, FileInput, FolderPlus, Layers3, LoaderCircle, Pause, Play, Plus, RotateCcw, Save, Trash2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { BatchInputRow, BatchRunRecord, BootstrapState, HistoryItem, ProjectChapter, ProjectMasterSettings, ProjectRecord, ProjectRenderBatch, SynthesisRequest, VoiceProfile } from "../types";
import { cancelBatchRun, deleteProject, exportProjectMaster, getBatchRun, importProjectScript, listHistory, loadGeneratedAudio, pauseBatchRun, pickProjectScript, queueBatchRun, resumeBatchRun, saveProject, synthesizeSpeech } from "../lib/bridge";
import { capabilityForModel, compatibleVoicesForModel, qualifiedModels } from "../lib/capabilities";
import { EmptyState, PageHeader, Panel, SelectField, StatusText } from "../components/ui";

function newChapter(position: number): ProjectChapter {
  return { id: crypto.randomUUID(), title: `Chapter ${position + 1}`, text: "", language: "en" };
}

export function reconcileProjectBatchChapters(
  chapters: ProjectChapter[],
  linkage: ProjectRenderBatch,
  batch: BatchRunRecord,
): ProjectChapter[] {
  const historyByIndex = new Map(batch.items.filter((item) => item.status === "completed" && item.history_id).map((item) => [item.item_index, item.history_id!]));
  const rowsByChapter = new Map(linkage.rows.map((row) => [row.chapter_id, row]));
  return chapters.map((chapter) => {
    const row = rowsByChapter.get(chapter.id);
    const historyId = row ? historyByIndex.get(row.item_index) : undefined;
    const sameRevision = row
      && chapter.text === row.source_text
      && (!("source_model_id" in row) || (chapter.model_id ?? null) === row.source_model_id)
      && (!("source_voice_id" in row) || (chapter.voice_id ?? null) === row.source_voice_id)
      && (!("source_language" in row) || (chapter.language ?? null) === row.source_language);
    return row && historyId && sameRevision && chapter.history_id !== historyId
      ? { ...chapter, history_id: historyId }
      : chapter;
  });
}

export function ProjectsView({
  bootstrap,
  projects,
  voices,
  onChange,
  onGenerated,
}: {
  bootstrap: BootstrapState;
  projects: ProjectRecord[];
  voices: VoiceProfile[];
  onChange: (projects: ProjectRecord[]) => void;
  onGenerated: (item: HistoryItem) => void;
}) {
  const ttsModels = useMemo(() => qualifiedModels(bootstrap, "tts"), [bootstrap]);
  const [selectedId, setSelectedId] = useState(projects[0]?.id ?? "");
  const [name, setName] = useState(projects[0]?.name ?? "Untitled project");
  const [chapters, setChapters] = useState<ProjectChapter[]>(projects[0]?.document.chapters ?? []);
  const [activeChapterId, setActiveChapterId] = useState(projects[0]?.document.chapters[0]?.id ?? "");
  const [state, setState] = useState<string>();
  const [renderingId, setRenderingId] = useState<string>();
  const [mastering, setMastering] = useState(false);
  const [masterSettings, setMasterSettings] = useState<ProjectMasterSettings>({ format: "wav", sample_rate: 48000, gap_ms: 250, fade_ms: 12, target_lufs: -16 });
  const [renderBatch, setRenderBatch] = useState<ProjectRenderBatch | undefined>(projects[0]?.document.render_batch);
  const [batchState, setBatchState] = useState<BatchRunRecord>();
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchPollVersion, setBatchPollVersion] = useState(0);
  const [parallelism, setParallelism] = useState(Math.min(2, bootstrap.scheduler.max_workers));
  const [audioUrl, setAudioUrl] = useState<string>();
  const [playing, setPlaying] = useState(false);
  const [undoStack, setUndoStack] = useState<ProjectChapter[][]>([]);
  const [redoStack, setRedoStack] = useState<ProjectChapter[][]>([]);
  const audioRef = useRef<HTMLAudioElement>(null);
  const deliveredHistoryIds = useRef(new Set<string>());
  const selectedIdRef = useRef(selectedId);
  const selected = projects.find((project) => project.id === selectedId);
  const activeIndex = chapters.findIndex((chapter) => chapter.id === activeChapterId);
  const active = chapters[activeIndex];
  const activeModel = ttsModels.find((model) => model.model_id === active?.model_id) ?? ttsModels.find((model) => model.engine === "kokoro") ?? ttsModels[0];
  const activeCapability = capabilityForModel(bootstrap, activeModel);
  const compatibleVoices = compatibleVoicesForModel(bootstrap, activeModel, voices);
  const selectableVoices = activeCapability?.voice_modes.includes("default")
    ? [{ value: "__engine_default__", label: `${activeCapability.display_name} default` }, ...compatibleVoices.map((voice) => ({ value: voice.id, label: voice.name }))]
    : compatibleVoices.map((voice) => ({ value: voice.id, label: voice.name }));
  const activeVoice = voices.find((voice) => voice.id === active?.voice_id);
  const referenceRequired = activeCapability?.voice_modes.length === 1 && activeCapability.voice_modes[0] === "reference";
  const staleChapters = chapters.filter((chapter) => chapter.text.trim() && !chapter.history_id);
  const batchActive = batchState && ["queued", "running", "paused"].includes(batchState.status);
  const batchItemsByChapter = new Map(renderBatch?.rows.map((row) => [row.chapter_id, batchState?.items.find((item) => item.item_index === row.item_index)]) ?? []);
  const activeBatchItem = active ? batchItemsByChapter.get(active.id) : undefined;
  const activeChapterIsQueued = Boolean(
    active
    && renderBatch?.rows.some((row) => row.chapter_id === active.id)
    && (!batchState || activeBatchItem && ["queued", "running"].includes(activeBatchItem.status)),
  );

  useEffect(() => () => { if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl); }, [audioUrl]);

  function loadProject(project: ProjectRecord) {
    selectedIdRef.current = project.id;
    setSelectedId(project.id);
    setName(project.name);
    setChapters(project.document.chapters);
    setActiveChapterId(project.document.chapters[0]?.id ?? "");
    setRenderBatch(project.document.render_batch);
    setBatchState(undefined);
    setUndoStack([]); setRedoStack([]); setState(undefined);
  }

  function createNew() {
    const first = newChapter(0);
    selectedIdRef.current = "";
    setSelectedId(""); setName("Untitled project"); setChapters([first]); setActiveChapterId(first.id);
    setRenderBatch(undefined); setBatchState(undefined);
    setUndoStack([]); setRedoStack([]); setState("New draft");
  }

  function commit(next: ProjectChapter[]) {
    setUndoStack((items) => [...items.slice(-39), chapters]);
    setRedoStack([]);
    setChapters(next);
    setState("Unsaved changes");
  }

  function updateActive(changes: Partial<ProjectChapter>) {
    if (!active) return;
    commit(chapters.map((chapter) => chapter.id === active.id
      ? { ...chapter, ...changes, history_id: changes.text !== undefined && changes.text !== chapter.text ? undefined : changes.history_id ?? chapter.history_id }
      : chapter));
  }

  function undo() {
    const prior = undoStack.at(-1); if (!prior) return;
    setRedoStack((items) => [...items, chapters]); setUndoStack((items) => items.slice(0, -1)); setChapters(prior);
  }

  function redo() {
    const next = redoStack.at(-1); if (!next) return;
    setUndoStack((items) => [...items, chapters]); setRedoStack((items) => items.slice(0, -1)); setChapters(next);
  }

  function addChapter() {
    const chapter = newChapter(chapters.length);
    commit([...chapters, chapter]); setActiveChapterId(chapter.id);
  }

  function moveChapter(direction: -1 | 1) {
    const destination = activeIndex + direction;
    if (!active || destination < 0 || destination >= chapters.length) return;
    const next = [...chapters]; [next[activeIndex], next[destination]] = [next[destination], next[activeIndex]]; commit(next);
  }

  async function persist(nextChapters = chapters, nextRenderBatch = renderBatch, projectId = selectedId || undefined) {
    if (!name.trim()) throw new Error("Project name is required");
    const expectedSelection = selectedIdRef.current;
    const next = await saveProject({
      ...selected,
      id: projectId,
      name: name.trim(),
      document: { script: nextChapters.map((chapter) => chapter.text).join("\n\n"), chapters: nextChapters, speaker_assignments: {}, render_batch: nextRenderBatch },
    });
    onChange([next, ...projects.filter((project) => project.id !== next.id)]);
    if (selectedIdRef.current === expectedSelection) {
      selectedIdRef.current = next.id;
      setSelectedId(next.id); setState("Saved locally");
    }
    return next;
  }

  async function reconcileBatch(batch: BatchRunRecord) {
    const projectId = selectedId;
    if (!renderBatch || batch.id !== renderBatch.batch_id || selectedIdRef.current !== projectId) return;
    const nextChapters = reconcileProjectBatchChapters(chapters, renderBatch, batch);
    const changed = nextChapters.some((chapter, index) => chapter !== chapters[index]);
    if (changed) {
      if (selectedIdRef.current !== projectId) return;
      setChapters(nextChapters);
      await persist(nextChapters, renderBatch, projectId);
    }
    const completedIds = batch.items.flatMap((item) => item.status === "completed" && item.history_id ? [item.history_id] : []);
    const unseen = completedIds.filter((id) => !deliveredHistoryIds.current.has(id));
    if (unseen.length) {
      const history = await listHistory();
      if (selectedIdRef.current !== projectId) return;
      history.filter((item) => unseen.includes(item.id)).forEach((item) => {
        deliveredHistoryIds.current.add(item.id);
        onGenerated(item);
      });
    }
  }

  useEffect(() => {
    if (!renderBatch) return;
    let disposed = false;
    let timer: number | undefined;
    const refresh = async () => {
      try {
        const batch = await getBatchRun(renderBatch.batch_id);
        if (disposed) return;
        setBatchState(batch);
        await reconcileBatch(batch);
        if (!disposed && ["queued", "running"].includes(batch.status)) timer = window.setTimeout(refresh, 750);
      } catch (caught) {
        if (!disposed) setState(caught instanceof Error ? caught.message : String(caught));
      }
    };
    void refresh();
    return () => { disposed = true; if (timer) window.clearTimeout(timer); };
  }, [renderBatch?.batch_id, batchPollVersion, chapters]);

  async function save() {
    try { await persist(); } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
  }

  async function renderChapter() {
    if (!active || !active.text.trim() || !activeModel || renderingId || activeChapterIsQueued) return;
    if (referenceRequired && !activeVoice?.local_path) { setState("This engine requires a clone-ready voice."); return; }
    setRenderingId(active.id); setState("Rendering selected chapter");
    try {
      const usesEngineDefault = activeCapability?.voice_modes.includes("default")
        && (!active.voice_id || active.voice_id === "__engine_default__");
      const voice = usesEngineDefault ? undefined : activeVoice ?? compatibleVoices[0];
      const result = await synthesizeSpeech({
        model_id: activeModel.model_id,
        text: active.text.trim(),
        input_mode: "text",
        speaker: activeModel.engine === "kokoro" ? voice?.id ?? "af_heart" : "default",
        language: active.language ?? activeCapability?.languages[0] ?? "en",
        reference_audio_path: voice?.state === "ready" ? voice.local_path : undefined,
        speed: 1,
        seed: 42817,
        output_format: "wav",
        title: `${name.trim()}: ${active.title}`,
        voice_name: voice?.name ?? `${activeCapability?.display_name ?? "Engine"} default`,
      });
      onGenerated(result);
      const nextChapters = chapters.map((chapter) => chapter.id === active.id ? { ...chapter, model_id: activeModel.model_id, voice_id: voice?.id, history_id: result.id } : chapter);
      setChapters(nextChapters);
      await persist(nextChapters);
      if (result.audio_path) {
        if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
        setAudioUrl(await loadGeneratedAudio(result.audio_path));
        window.setTimeout(() => void audioRef.current?.play(), 0);
      }
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setRenderingId(undefined); }
  }

  function chapterBatchRow(chapter: ProjectChapter, itemIndex: number): BatchInputRow {
    const model = ttsModels.find((candidate) => candidate.model_id === chapter.model_id)
      ?? ttsModels.find((candidate) => candidate.engine === "kokoro")
      ?? ttsModels[0];
    if (!model) throw new Error(`${chapter.title || `Chapter ${itemIndex + 1}`} has no installed TTS model.`);
    const capability = capabilityForModel(bootstrap, model);
    const voicesForModel = compatibleVoicesForModel(bootstrap, model, voices);
    const usesEngineDefault = capability?.voice_modes.includes("default")
      && (!chapter.voice_id || chapter.voice_id === "__engine_default__");
    const voice = usesEngineDefault
      ? undefined
      : voicesForModel.find((candidate) => candidate.id === chapter.voice_id) ?? voicesForModel[0];
    const needsReference = capability?.voice_modes.length === 1 && capability.voice_modes[0] === "reference";
    if (needsReference && !voice?.local_path) {
      throw new Error(`${chapter.title || `Chapter ${itemIndex + 1}`} requires a clone-ready voice.`);
    }
    const settings: Partial<SynthesisRequest> = {
      model_id: model.model_id,
      input_mode: "text",
      speaker: model.engine === "kokoro" ? voice?.id ?? "af_heart" : "default",
      language: chapter.language ?? capability?.languages[0] ?? "en",
      reference_audio_path: voice?.state === "ready" ? voice.local_path : undefined,
      voice_name: voice?.name ?? `${capability?.display_name ?? "Engine"} default`,
      speed: 1,
      seed: 42817 + itemIndex,
      output_format: "wav",
    };
    return {
      text: chapter.text.trim(),
      name: `${name.trim()}: ${chapter.title || `Chapter ${itemIndex + 1}`}`,
      output_name: `project-${chapter.id.slice(0, 12)}`,
      settings,
    };
  }

  async function renderStaleChapters() {
    if (!staleChapters.length || batchBusy) return;
    setBatchBusy(true);
    setState("Validating stale chapters");
    try {
      const rows = staleChapters.map(chapterBatchRow);
      const saved = await persist(chapters, undefined);
      const batch = await queueBatchRun(
        `${saved.name} / stale chapters`,
        rows,
        { model_id: rows[0].settings?.model_id },
        parallelism,
        "normal",
      );
      const linkage: ProjectRenderBatch = {
        batch_id: batch.id,
        started_at: new Date().toISOString(),
        rows: staleChapters.map((chapter, item_index) => ({
          chapter_id: chapter.id,
          item_index,
          source_text: chapter.text,
          source_model_id: chapter.model_id ?? null,
          source_voice_id: chapter.voice_id ?? null,
          source_language: chapter.language ?? null,
        })),
      };
      setRenderBatch(linkage);
      setBatchState(batch);
      await persist(chapters, linkage, saved.id);
      setState(`Queued ${rows.length} stale chapter${rows.length === 1 ? "" : "s"}`);
    } catch (caught) {
      setState(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBatchBusy(false);
    }
  }

  async function pauseProjectBatch() {
    if (!batchState || batchBusy) return;
    setBatchBusy(true);
    try { setBatchState(await pauseBatchRun(batchState.id)); setState("Project rendering paused"); }
    catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setBatchBusy(false); }
  }

  async function resumeProjectBatch() {
    if (!batchState || batchBusy) return;
    setBatchBusy(true);
    try {
      const batch = await resumeBatchRun(batchState.id, parallelism, batchState.status === "failed");
      setBatchState(batch); setBatchPollVersion((value) => value + 1); setState(batchState.status === "failed" ? "Retrying failed chapters" : "Project rendering resumed");
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setBatchBusy(false); }
  }

  async function cancelProjectBatch() {
    if (!batchState || batchBusy) return;
    setBatchBusy(true);
    try { setBatchState(await cancelBatchRun(batchState.id)); setState("Project rendering cancelled"); }
    catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setBatchBusy(false); }
  }

  async function removeProject() {
    if (!selected || !window.confirm(`Delete project “${selected.name}”? Generated audio will remain in History.`)) return;
    try {
      if (await deleteProject(selected.id)) {
        const remaining = projects.filter((project) => project.id !== selected.id); onChange(remaining);
        if (remaining[0]) loadProject(remaining[0]); else createNew();
      }
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
  }

  async function importScript() {
    try {
      const path = await pickProjectScript();
      if (!path) return;
      setState("Importing script");
      const project = await importProjectScript(path);
      onChange([project, ...projects.filter((item) => item.id !== project.id)]);
      loadProject(project);
      setState("Script imported locally");
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
  }

  async function masterProject() {
    if (!selectedId || mastering) return;
    setMastering(true); setState("Building project master");
    try {
      const result = await exportProjectMaster(selectedId, masterSettings);
      onGenerated(result.history);
      if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
      setAudioUrl(await loadGeneratedAudio(result.history.audio_path ?? ""));
      setState(`Master exported with provenance: ${result.export.manifest_path}`);
      window.setTimeout(() => void audioRef.current?.play(), 0);
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setMastering(false); }
  }

  const writtenChapters = chapters.filter((chapter) => chapter.text.trim());
  const canMaster = Boolean(selectedId) && writtenChapters.length > 0 && writtenChapters.every((chapter) => chapter.history_id);

  return <div className="page projects-page">
    <PageHeader title="Projects" subtitle="Build long-form work from independently editable, selectively rendered chapters." actions={<><button className="button button-secondary" type="button" disabled={!undoStack.length} onClick={undo}>Undo</button><button className="button button-secondary" type="button" disabled={!redoStack.length} onClick={redo}>Redo</button><button className="button button-secondary" type="button" onClick={() => void importScript()}><FileInput size={14} />Import</button><button className="button button-primary" type="button" onClick={createNew}><FolderPlus size={14} />New project</button></>} />
    <div className="projects-layout">
      <Panel className="project-list table-panel" ariaLabel="Project list">
        <div className="project-list-heading"><span className="section-label">Projects</span><button className="icon-button" title="Add project" type="button" onClick={createNew}><Plus size={13} /></button></div>
        <div className="project-rows">{projects.length ? projects.map((project) => <button className={`project-row ${project.id === selectedId ? "is-selected" : ""}`} key={project.id} type="button" onClick={() => loadProject(project)}><strong>{project.name}</strong><span>{project.document.chapters.length} chapters / {project.document.chapters.filter((chapter) => chapter.history_id).length} rendered</span><small>{new Date(project.updated_at).toLocaleString()}</small></button>) : <EmptyState title="No projects yet" detail="Create a project to begin a long-form production." />}</div>
      </Panel>
      <Panel className="project-studio" ariaLabel="Project studio">
        <div className="project-studio-bar"><label className="form-field project-name"><span>Project name</span><input value={name} onChange={(event) => { setName(event.target.value); setState("Unsaved changes"); }} /></label><button className="button button-secondary" type="button" onClick={addChapter}><Plus size={13} />Chapter</button><button className="button button-primary" type="button" onClick={() => void save()}><Save size={13} />Save</button></div>
        <div className="chapter-workspace">
          <div className="chapter-list"><div className="chapter-list-tools"><span className="section-label">Chapters</span><div><button className="icon-button" title="Move chapter up" type="button" disabled={activeIndex <= 0} onClick={() => moveChapter(-1)}><ChevronUp size={12} /></button><button className="icon-button" title="Move chapter down" type="button" disabled={activeIndex < 0 || activeIndex >= chapters.length - 1} onClick={() => moveChapter(1)}><ChevronDown size={12} /></button></div></div>{chapters.map((chapter, index) => {
            const batchItem = batchItemsByChapter.get(chapter.id);
            const label = chapter.history_id ? "Rendered" : batchItem?.status === "running" ? "Rendering" : batchItem?.status === "queued" ? "Queued" : batchItem?.status === "failed" ? "Failed" : batchItem?.status === "cancelled" ? "Cancelled" : chapter.text.trim() ? "Stale" : "Empty";
            const tone = chapter.history_id ? "success" : batchItem?.status === "failed" ? "danger" : chapter.text.trim() ? "warning" : "muted";
            return <button className={`chapter-row ${chapter.id === activeChapterId ? "is-selected" : ""}`} type="button" key={chapter.id} onClick={() => setActiveChapterId(chapter.id)}><span>{String(index + 1).padStart(2, "0")}</span><div><strong>{chapter.title || "Untitled chapter"}</strong><small title={batchItem?.error ?? undefined}>{batchItem?.error ?? `${chapter.text.length} characters`}</small></div><StatusText tone={tone}>{label}</StatusText></button>;
          })}</div>
          {active ? <div className="chapter-editor"><div className="chapter-title-row"><label className="form-field"><span>Chapter title</span><input value={active.title} onChange={(event) => updateActive({ title: event.target.value })} /></label><button className="icon-button danger-button" title="Delete chapter" type="button" disabled={chapters.length <= 1} onClick={() => { const next = chapters.filter((chapter) => chapter.id !== active.id); commit(next); setActiveChapterId(next[Math.max(0, activeIndex - 1)]?.id ?? ""); }}><Trash2 size={13} /></button></div><label className="form-field project-script"><span>Script</span><textarea value={active.text} onChange={(event) => updateActive({ text: event.target.value })} placeholder="Write this chapter..." /></label><div className="chapter-settings"><SelectField label="Model" value={activeModel?.model_id ?? ""} onChange={(model_id) => updateActive({ model_id, voice_id: undefined, history_id: undefined })} options={ttsModels.map((model) => ({ value: model.model_id, label: model.model_id }))} /><SelectField label="Voice" value={active.voice_id ?? selectableVoices[0]?.value ?? ""} onChange={(voice_id) => updateActive({ voice_id, history_id: undefined })} options={selectableVoices} /><SelectField label="Language" value={active.language ?? activeCapability?.languages[0] ?? "en"} onChange={(language) => updateActive({ language, history_id: undefined })} options={(activeCapability?.languages ?? ["en"]).map((language) => ({ value: language, label: language.toUpperCase() }))} /></div><div className="chapter-actions"><StatusText tone={state === "Saved locally" ? "success" : state?.includes("requires") ? "danger" : "muted"}>{activeChapterIsQueued ? "Queued in chapter render" : state ?? (active.history_id ? "Rendered artifact linked" : "Ready to render")}</StatusText>{audioUrl ? <audio className="visually-hidden" ref={audioRef} src={audioUrl} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} /> : null}<button className="icon-button" title={playing ? "Pause chapter" : "Play latest chapter"} type="button" disabled={!audioUrl} onClick={() => audioRef.current && (audioRef.current.paused ? void audioRef.current.play() : audioRef.current.pause())}>{playing ? <Pause size={13} /> : <Play size={13} />}</button><button className="button button-primary" type="button" disabled={!active.text.trim() || !activeModel || Boolean(renderingId) || activeChapterIsQueued || (referenceRequired && !activeVoice?.local_path)} onClick={() => void renderChapter()}>{renderingId ? <LoaderCircle className="spin" size={13} /> : <Play size={13} />}{renderingId ? "Rendering" : active.history_id ? "Regenerate clip" : "Render clip"}</button></div></div> : <EmptyState title="No chapter selected" detail="Add a chapter to begin writing." />}
        </div>
        <div className="project-render-bar">
          <div className="project-render-summary"><Layers3 size={14} /><span><strong>Chapter queue</strong><small>{batchState ? `${batchState.completed_items}/${batchState.total_items} complete${batchState.failed_items ? ` / ${batchState.failed_items} failed` : ""} / ${batchState.status}` : `${staleChapters.length} stale chapter${staleChapters.length === 1 ? "" : "s"}`}</small></span></div>
          <SelectField label="Parallel" value={String(parallelism)} onChange={(value) => setParallelism(Number(value))} disabled={Boolean(batchActive)} options={Array.from({ length: Math.min(4, bootstrap.scheduler.max_workers) }, (_, index) => ({ value: String(index + 1), label: String(index + 1) }))} />
          {batchState && ["queued", "running"].includes(batchState.status) ? <button className="icon-button" title="Pause project rendering" type="button" disabled={batchBusy} onClick={() => void pauseProjectBatch()}><Pause size={13} /></button> : null}
          {batchState && ["paused", "failed"].includes(batchState.status) ? <button className="button button-secondary" type="button" disabled={batchBusy} onClick={() => void resumeProjectBatch()}><RotateCcw size={13} />{batchState.status === "failed" ? "Retry failed" : "Resume"}</button> : null}
          {batchActive ? <button className="icon-button danger-button" title="Cancel project rendering" type="button" disabled={batchBusy} onClick={() => void cancelProjectBatch()}><CircleStop size={13} /></button> : null}
          <button className="button button-primary" type="button" disabled={!staleChapters.length || Boolean(batchActive) || batchBusy} onClick={() => void renderStaleChapters()}>{batchBusy ? <LoaderCircle className="spin" size={13} /> : <Layers3 size={13} />}{batchBusy ? "Updating" : `Render stale${staleChapters.length ? ` (${staleChapters.length})` : ""}`}</button>
        </div>
        <div className="project-master-bar">
          <span className="section-label">Master</span>
          <SelectField label="Format" value={masterSettings.format} onChange={(format) => setMasterSettings((current) => ({ ...current, format: format as "wav" | "flac" }))} options={[{ value: "wav", label: "WAV" }, { value: "flac", label: "FLAC" }]} />
          <SelectField label="Rate" value={String(masterSettings.sample_rate)} onChange={(sample_rate) => setMasterSettings((current) => ({ ...current, sample_rate: Number(sample_rate) as ProjectMasterSettings["sample_rate"] }))} options={[{ value: "24000", label: "24 kHz" }, { value: "44100", label: "44.1 kHz" }, { value: "48000", label: "48 kHz" }]} />
          <label className="form-field"><span>Gap</span><input type="number" min="0" max="5000" step="50" value={masterSettings.gap_ms} onChange={(event) => setMasterSettings((current) => ({ ...current, gap_ms: Number(event.target.value) }))} /></label>
          <label className="form-field"><span>LUFS</span><input type="number" min="-24" max="-9" step="1" value={masterSettings.target_lufs} onChange={(event) => setMasterSettings((current) => ({ ...current, target_lufs: Number(event.target.value) }))} /></label>
          <button className="button button-primary" title={canMaster ? "Export mastered project" : "Render every written chapter before mastering"} type="button" disabled={!canMaster || mastering} onClick={() => void masterProject()}>{mastering ? <LoaderCircle className="spin" size={13} /> : <Download size={13} />}{mastering ? "Mastering" : "Export master"}</button>
        </div>
        <div className="project-editor-footer"><StatusText tone={state?.startsWith("Master exported") ? "success" : "muted"}>{state?.startsWith("Master exported") ? state : `${chapters.length} chapters / ${chapters.filter((chapter) => chapter.history_id).length} rendered / ${chapters.filter((chapter) => chapter.text.trim() && !chapter.history_id).length} stale`}</StatusText><button className="button button-secondary danger-button" type="button" disabled={!selected} onClick={() => void removeProject()}><Trash2 size={13} />Delete project</button></div>
      </Panel>
    </div>
  </div>;
}
