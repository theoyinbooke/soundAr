import { ArrowLeft, AudioLines, ChevronDown, ChevronUp, Clapperboard, CircleStop, Download, FileInput, FolderPlus, Layers3, LoaderCircle, Pause, Play, Plus, Redo2, RotateCcw, Save, Search, Trash2, Undo2, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { BatchInputRow, BatchRunRecord, BootstrapState, HistoryItem, ProjectChapter, ProjectMasterSettings, ProjectRecord, ProjectRenderBatch, SynthesisRequest, VoiceProfile } from "../types";
import { cancelBatchRun, deleteProject, exportHistoryItem, exportProjectMaster, getBatchRun, importProjectScript, listHistory, loadGeneratedAudio, pauseBatchRun, pickProjectScript, queueBatchRun, resumeBatchRun, saveProject, synthesizeSpeech } from "../lib/bridge";
import { capabilityForModel, compatibleVoicesForModel, qualifiedModels } from "../lib/capabilities";
import { EmptyState, PageHeader, Panel, SelectField, StatusText } from "../components/ui";
import { useVideoIntegration, useVideoProjectSummaries } from "../components/video/VideoIntegrationContext";
import { sortVideoProjectsForLibrary, videoProjectStatusLabel } from "../components/video/VideoMasterCard";

/** One production in the unified table, whichever workspace it opens in. */
type ProjectTableRow = {
  id: string;
  kind: "video" | "audio";
  name: string;
  detail: string;
  status: string;
  updatedAt: string;
  open: () => void;
};

function newChapter(position: number): ProjectChapter {
  return { id: crypto.randomUUID(), title: `Chapter ${position + 1}`, text: "", language: "en" };
}

function formatDuration(seconds = 0) {
  const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
  return `${Math.floor(safe / 60)}:${Math.floor(safe % 60).toString().padStart(2, "0")}`;
}

function ProjectSetupDialog({
  initialName = "",
  mode,
  onClose,
  onImport,
  onSubmit,
}: {
  initialName?: string;
  mode: "create" | "rename";
  onClose: () => void;
  onImport?: () => void;
  onSubmit: (name: string) => void;
}) {
  const [value, setValue] = useState(initialName);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
    const closeOnEscape = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return <div className="modal-backdrop" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
    <form className="modal project-setup-dialog" role="dialog" aria-modal="true" aria-labelledby="project-setup-title" onSubmit={(event) => { event.preventDefault(); if (value.trim()) onSubmit(value.trim()); }}>
      <header className="modal-header">
        <div><h2 id="project-setup-title">{mode === "create" ? "Create project" : "Rename project"}</h2><p>{mode === "create" ? "Set up a focused workspace for a long-form voice production." : "Update the name shown in your local project library."}</p></div>
        <button className="icon-button" type="button" aria-label="Close project setup" onClick={onClose}><X aria-hidden="true" size={15} /></button>
      </header>
      <div className="modal-body">
        <label className="form-field"><span>Project name</span><input ref={inputRef} value={value} onChange={(event) => setValue(event.target.value)} placeholder="e.g. Product launch narration" /></label>
        {mode === "create" ? <><div className="project-dialog-note"><FolderPlus aria-hidden="true" size={16} /><span><strong>Local by default</strong><small>Scripts, chapter settings, and rendered audio stay on this computer.</small></span></div><button className="project-dialog-import" type="button" onClick={onImport}><FileInput aria-hidden="true" size={14} /><span><strong>Import an existing script</strong><small>Start a project from a TXT, Markdown, or Fountain document.</small></span></button></> : null}
      </div>
      <footer className="modal-actions"><button className="button button-secondary" type="button" onClick={onClose}>Cancel</button><button className="button button-primary" type="submit" disabled={!value.trim()}>{mode === "create" ? "Create project" : "Save name"}</button></footer>
    </form>
  </div>;
}

function ChapterSetupDialog({ position, onClose, onSubmit }: { position: number; onClose: () => void; onSubmit: (title: string, text: string) => void }) {
  const [title, setTitle] = useState(`Chapter ${position + 1}`);
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return <div className="modal-backdrop" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
    <form className="modal chapter-setup-dialog" role="dialog" aria-modal="true" aria-labelledby="chapter-setup-title" onSubmit={(event) => { event.preventDefault(); onSubmit(title.trim() || `Chapter ${position + 1}`, text); }}>
      <header className="modal-header"><div><h2 id="chapter-setup-title">Add chapter</h2><p>Start with a title and, optionally, paste the first draft.</p></div><button className="icon-button" type="button" aria-label="Close chapter setup" onClick={onClose}><X aria-hidden="true" size={15} /></button></header>
      <div className="modal-body"><label className="form-field"><span>Chapter title</span><input ref={inputRef} value={title} onChange={(event) => setTitle(event.target.value)} /></label><label className="form-field"><span>Starting script <small>Optional</small></span><textarea value={text} onChange={(event) => setText(event.target.value)} placeholder="Paste a draft or leave this blank to begin in the editor." /></label></div>
      <footer className="modal-actions"><button className="button button-secondary" type="button" onClick={onClose}>Cancel</button><button className="button button-primary" type="submit">Add chapter</button></footer>
    </form>
  </div>;
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
  const { onOpenProject: onOpenVideoProject, service: videoService } = useVideoIntegration();
  const { projects: videoProjects, loading: videoProjectsLoading, error: videoProjectsError } = useVideoProjectSummaries();
  const ttsModels = useMemo(() => qualifiedModels(bootstrap, "tts"), [bootstrap]);
  // Nothing is open until the user opens it. The page is a library first and a workspace second,
  // so landing on it must not silently load whichever project happens to sort first.
  const [selectedId, setSelectedId] = useState("");
  const [name, setName] = useState("Untitled project");
  const [chapters, setChapters] = useState<ProjectChapter[]>([]);
  const [activeChapterId, setActiveChapterId] = useState("");
  const [state, setState] = useState<string>();
  const [renderingId, setRenderingId] = useState<string>();
  const [mastering, setMastering] = useState(false);
  const [masterSettings, setMasterSettings] = useState<ProjectMasterSettings>({ format: "wav", sample_rate: 48000, gap_ms: 250, fade_ms: 12, target_lufs: -16 });
  const [renderBatch, setRenderBatch] = useState<ProjectRenderBatch | undefined>();
  const [batchState, setBatchState] = useState<BatchRunRecord>();
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchPollVersion, setBatchPollVersion] = useState(0);
  const [parallelism, setParallelism] = useState(Math.min(2, bootstrap.scheduler.max_workers));
  const [audioUrl, setAudioUrl] = useState<string>();
  const [playing, setPlaying] = useState(false);
  const [masterAudioUrl, setMasterAudioUrl] = useState<string>();
  const [masterPlaying, setMasterPlaying] = useState(false);
  const [masterTime, setMasterTime] = useState(0);
  const [masterDuration, setMasterDuration] = useState(0);
  const [masterHistory, setMasterHistory] = useState<HistoryItem>();
  const [undoStack, setUndoStack] = useState<ProjectChapter[][]>([]);
  const [redoStack, setRedoStack] = useState<ProjectChapter[][]>([]);
  const [search, setSearch] = useState("");
  const [kindFilter, setKindFilter] = useState<"all" | "video" | "audio">("all");
  const [projectDialogMode, setProjectDialogMode] = useState<"create" | "rename">();
  const [chapterDialogOpen, setChapterDialogOpen] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);
  const masterAudioRef = useRef<HTMLAudioElement>(null);
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

  useEffect(() => {
    const master = selected?.document.master;
    let active = true;
    let objectUrl: string | undefined;
    setMasterHistory(undefined);
    setMasterAudioUrl(undefined);
    setMasterTime(0);
    setMasterDuration(master?.duration_seconds ?? 0);
    if (!master?.audio_path || !master.history_id) return () => undefined;
    void Promise.all([listHistory(), loadGeneratedAudio(master.audio_path)]).then(([history, source]) => {
      if (!active) return;
      objectUrl = source;
      setMasterHistory(history.find((item) => item.id === master.history_id));
      setMasterAudioUrl(source);
    }).catch((caught) => active && setState(caught instanceof Error ? caught.message : String(caught)));
    return () => { active = false; if (objectUrl?.startsWith("blob:")) URL.revokeObjectURL(objectUrl); };
  }, [selected?.id, selected?.document.master?.history_id, selected?.document.master?.audio_path]);

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

  // Leaving a project returns to the library rather than to another project, so "back" means the
  // list every time instead of whichever record happened to be next.
  function closeProject() {
    selectedIdRef.current = "";
    setSelectedId(""); setName("Untitled project"); setChapters([]); setActiveChapterId("");
    setRenderBatch(undefined); setBatchState(undefined);
    setUndoStack([]); setRedoStack([]); setState(undefined);
  }

  function createNew(projectName = "Untitled project") {
    const first = newChapter(0);
    selectedIdRef.current = "";
    setSelectedId(""); setName(projectName); setChapters([first]); setActiveChapterId(first.id);
    setRenderBatch(undefined); setBatchState(undefined);
    setUndoStack([]); setRedoStack([]); setState("New draft");
    setProjectDialogMode(undefined);
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

  function addChapter(title = `Chapter ${chapters.length + 1}`, text = "") {
    const chapter = { ...newChapter(chapters.length), title, text };
    commit([...chapters, chapter]); setActiveChapterId(chapter.id);
    setChapterDialogOpen(false);
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
      document: { ...(selected?.document ?? {}), script: nextChapters.map((chapter) => chapter.text).join("\n\n"), chapters: nextChapters, speaker_assignments: selected?.document.speaker_assignments ?? {}, render_batch: nextRenderBatch },
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
        onChange(projects.filter((project) => project.id !== selected.id));
        // A deleted project has no screen to stay on.
        closeProject();
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
      onChange([result.project, ...projects.filter((project) => project.id !== result.project.id)]);
      loadProject(result.project);
      if (masterAudioUrl?.startsWith("blob:")) URL.revokeObjectURL(masterAudioUrl);
      setMasterHistory(result.history);
      setMasterAudioUrl(await loadGeneratedAudio(result.history.audio_path ?? ""));
      setState(`Master exported with provenance: ${result.export.manifest_path}`);
      window.setTimeout(() => void masterAudioRef.current?.play(), 0);
    } catch (caught) { setState(caught instanceof Error ? caught.message : String(caught)); }
    finally { setMastering(false); }
  }

  const writtenChapters = chapters.filter((chapter) => chapter.text.trim());
  const canMaster = Boolean(selectedId) && writtenChapters.length > 0 && writtenChapters.every((chapter) => chapter.history_id);

  const workspaceOpen = Boolean(selected || chapters.length || state === "New draft");
  // One table for everything the user has made. A video production and an audio production are
  // different experiences to open, but they are the same thing to look for, so splitting them into
  // separate lists made the page a place to hunt rather than a place to choose.
  const allRows = useMemo<ProjectTableRow[]>(() => {
    const audioRows = projects.map<ProjectTableRow>((project) => ({
      id: project.id,
      kind: "audio",
      name: project.name,
      detail: `${project.document.chapters.length} chapter${project.document.chapters.length === 1 ? "" : "s"} · ${project.document.chapters.filter((chapter) => chapter.history_id).length} rendered`,
      status: project.document.master ? "Mastered" : project.document.chapters.some((chapter) => chapter.history_id) ? "In progress" : "Draft",
      updatedAt: project.updated_at,
      open: () => loadProject(project),
    }));
    const videoRows = sortVideoProjectsForLibrary(videoProjects).map<ProjectTableRow>((project) => ({
      id: project.id,
      kind: "video",
      name: project.name,
      detail: `${project.scene_count} scene${project.scene_count === 1 ? "" : "s"} · ${formatDuration(project.duration_ms / 1000)}`,
      status: videoProjectStatusLabel(project),
      updatedAt: project.updated_at,
      // Video Studio is only wired up when its service is present; without it the row is inert
      // rather than pretending it can open something.
      open: () => onOpenVideoProject?.(project.id),
    }));
    return [...videoRows, ...audioRows].sort(
      (left, right) => Date.parse(right.updatedAt) - Date.parse(left.updatedAt),
    );
  }, [projects, videoProjects, onOpenVideoProject]);

  const filteredRows = allRows.filter((row) => {
    if (kindFilter !== "all" && row.kind !== kindFilter) return false;
    const term = search.trim().toLowerCase();
    return !term || row.name.toLowerCase().includes(term);
  });

  // The library and one project are two screens, not one page with a drawer. A project gets the
  // whole surface, because that is where the work happens.
  return <>
    {workspaceOpen ? <div className="page project-page">
      <PageHeader
        title={name || "Untitled project"}
        subtitle={selected ? `${chapters.length} chapter${chapters.length === 1 ? "" : "s"} · ${chapters.filter((chapter) => chapter.history_id).length} rendered` : "New draft, not saved yet"}
        actions={<>
          <button className="button button-secondary" type="button" onClick={closeProject}><ArrowLeft aria-hidden="true" size={14} />All projects</button>
          <button className="button button-secondary" type="button" onClick={() => setProjectDialogMode("rename")}>Rename</button>
        </>}
      />
      <Panel className="project-studio" ariaLabel="Project workspace">
        <header className="project-document-header">
          <StatusText tone={state?.includes("requires") || state?.includes("failed") ? "danger" : "muted"}>{state ?? "Saved"}</StatusText>
          <div className="project-document-actions">
            <button className="icon-button" aria-label="Undo chapter edit" title="Undo" type="button" disabled={!undoStack.length} onClick={undo}><Undo2 aria-hidden="true" size={14} /></button>
            <button className="icon-button" aria-label="Redo chapter edit" title="Redo" type="button" disabled={!redoStack.length} onClick={redo}><Redo2 aria-hidden="true" size={14} /></button>
            <button className="button button-secondary" type="button" onClick={() => setChapterDialogOpen(true)}><Plus aria-hidden="true" size={13} />Add chapter</button>
            <button className="button button-primary" type="button" onClick={() => void save()}><Save aria-hidden="true" size={13} />Save</button>
          </div>
        </header>

        <div className="chapter-workspace">
          <aside className="chapter-list">
            <div className="chapter-list-tools"><span><strong>Chapters</strong><small>· {chapters.length}</small></span><div><button className="icon-button" aria-label="Move chapter up" title="Move chapter up" type="button" disabled={activeIndex <= 0} onClick={() => moveChapter(-1)}><ChevronUp aria-hidden="true" size={12} /></button><button className="icon-button" aria-label="Move chapter down" title="Move chapter down" type="button" disabled={activeIndex < 0 || activeIndex >= chapters.length - 1} onClick={() => moveChapter(1)}><ChevronDown aria-hidden="true" size={12} /></button></div></div>
            <div className="chapter-rows">{chapters.map((chapter) => {
              const batchItem = batchItemsByChapter.get(chapter.id);
              const label = chapter.history_id ? "Rendered" : batchItem?.status === "running" ? "Rendering" : batchItem?.status === "queued" ? "Queued" : batchItem?.status === "failed" ? "Failed" : batchItem?.status === "cancelled" ? "Cancelled" : chapter.text.trim() ? "Changed" : "Empty";
              const tone = batchItem?.status === "failed" ? "danger" : "muted";
              return <button className={`chapter-row ${chapter.id === activeChapterId ? "is-selected" : ""}`} type="button" key={chapter.id} onClick={() => setActiveChapterId(chapter.id)}><div><strong>{chapter.title || "Untitled chapter"}</strong><small title={batchItem?.error ?? undefined}>{batchItem?.error ?? `${chapter.text.length} characters`}</small></div><StatusText tone={tone}>{label}</StatusText></button>;
            })}</div>
            <button className="chapter-add-row" type="button" onClick={() => setChapterDialogOpen(true)}><Plus aria-hidden="true" size={13} />Add chapter</button>
          </aside>

          {active ? <article className="chapter-editor">
            <div className="chapter-title-row"><label><span className="visually-hidden">Chapter title</span><input className="chapter-title-input" value={active.title} onChange={(event) => updateActive({ title: event.target.value })} /></label><button className="icon-button danger-button" aria-label="Delete chapter" title="Delete chapter" type="button" disabled={chapters.length <= 1} onClick={() => { const next = chapters.filter((chapter) => chapter.id !== active.id); commit(next); setActiveChapterId(next[Math.max(0, activeIndex - 1)]?.id ?? ""); }}><Trash2 aria-hidden="true" size={13} /></button></div>
            <label className="project-script"><span className="visually-hidden">Script</span><textarea aria-label="Script" value={active.text} onChange={(event) => updateActive({ text: event.target.value })} placeholder="Write this chapter…" /></label>
            <details className="chapter-voice-settings"><summary><span><strong>Voice and model</strong><small>{activeModel?.model_id.split("/").at(-1) ?? "No model"} · {activeVoice?.name ?? "Engine default"}</small></span><ChevronDown aria-hidden="true" size={14} /></summary><div className="chapter-settings"><SelectField label="Model" value={activeModel?.model_id ?? ""} onChange={(model_id) => updateActive({ model_id, voice_id: undefined, history_id: undefined })} options={ttsModels.map((model) => ({ value: model.model_id, label: model.model_id }))} /><SelectField label="Voice" value={active.voice_id ?? selectableVoices[0]?.value ?? ""} onChange={(voice_id) => updateActive({ voice_id, history_id: undefined })} options={selectableVoices} /><SelectField label="Language" value={active.language ?? activeCapability?.languages[0] ?? "en"} onChange={(language) => updateActive({ language, history_id: undefined })} options={(activeCapability?.languages ?? ["en"]).map((language) => ({ value: language, label: language.toUpperCase() }))} /></div></details>
            <div className="chapter-actions"><StatusText tone={state?.includes("requires") ? "danger" : "muted"}>{activeChapterIsQueued ? "Queued in project render" : active.history_id ? "Rendered clip linked" : active.text.trim() ? "Ready to render" : "Start writing to enable rendering"}</StatusText>{audioUrl ? <audio className="visually-hidden" ref={audioRef} src={audioUrl} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} /> : null}<button className="icon-button" aria-label={playing ? "Pause chapter" : "Play latest chapter"} title={playing ? "Pause chapter" : "Play latest chapter"} type="button" disabled={!audioUrl} onClick={() => audioRef.current && (audioRef.current.paused ? void audioRef.current.play() : audioRef.current.pause())}>{playing ? <Pause aria-hidden="true" size={13} /> : <Play aria-hidden="true" size={13} />}</button><button className="button button-primary" type="button" disabled={!active.text.trim() || !activeModel || Boolean(renderingId) || activeChapterIsQueued || (referenceRequired && !activeVoice?.local_path)} onClick={() => void renderChapter()}>{renderingId ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <Play aria-hidden="true" size={13} />}{renderingId ? "Rendering" : active.history_id ? "Regenerate" : "Render chapter"}</button></div>
          </article> : <div className="project-empty-editor"><EmptyState title="Add your first chapter" detail="Chapters keep long-form scripts focused and independently renderable." action={<button className="button button-primary" type="button" onClick={() => setChapterDialogOpen(true)}>Add chapter</button>} /></div>}
        </div>

        <details className="project-production-drawer">
          <summary><span><strong>Production and export</strong><small>{batchState ? `${batchState.completed_items}/${batchState.total_items} rendered · ${batchState.status}` : `${staleChapters.length} chapter${staleChapters.length === 1 ? "" : "s"} ready to render`}</small></span><ChevronDown aria-hidden="true" size={15} /></summary>
          <div className="project-production-content">
            {selected?.document.master ? <section className="project-master-artifact" aria-label="Project master audio">
              <audio ref={masterAudioRef} src={masterAudioUrl} onPlay={() => setMasterPlaying(true)} onPause={() => setMasterPlaying(false)} onEnded={() => setMasterPlaying(false)} onLoadedMetadata={(event) => setMasterDuration(Number.isFinite(event.currentTarget.duration) && event.currentTarget.duration > 0 ? event.currentTarget.duration : selected.document.master?.duration_seconds ?? 0)} onTimeUpdate={(event) => setMasterTime(event.currentTarget.currentTime)} />
              <button className="project-master-play" type="button" aria-label={masterPlaying ? "Pause project master" : "Play project master"} disabled={!masterAudioUrl} onClick={() => masterAudioRef.current && (masterAudioRef.current.paused ? void masterAudioRef.current.play() : masterAudioRef.current.pause())}>{masterPlaying ? <Pause aria-hidden="true" size={14} /> : <Play aria-hidden="true" size={14} />}</button>
              <div className="project-master-main"><strong>{selected.document.master.title || `${selected.name} master`}</strong><small>Project master · {formatDuration(masterDuration || selected.document.master.duration_seconds)} · {(selected.document.master.format || "wav").toUpperCase()} {selected.document.master.sample_rate ? `${Math.round(selected.document.master.sample_rate / 1000)} kHz` : ""}</small><input aria-label="Project master position" type="range" min={0} max={Math.max(masterDuration || selected.document.master.duration_seconds || 0, 0.01)} step={0.05} value={Math.min(masterTime, masterDuration || selected.document.master.duration_seconds || 0)} onChange={(event) => { if (masterAudioRef.current) masterAudioRef.current.currentTime = Number(event.target.value); }} /></div>
              <button className="button button-secondary" type="button" disabled={!masterHistory} onClick={() => masterHistory && void exportHistoryItem(masterHistory)}><Download aria-hidden="true" size={13} />Export copy</button>
            </section> : null}
            <section className="project-render-section"><div className="project-render-summary"><Layers3 aria-hidden="true" size={14} /><span><strong>Chapter queue</strong><small>Render changed chapters together without blocking the editor.</small></span></div><SelectField label="Parallel jobs" value={String(parallelism)} onChange={(value) => setParallelism(Number(value))} disabled={Boolean(batchActive)} options={Array.from({ length: Math.min(4, bootstrap.scheduler.max_workers) }, (_, index) => ({ value: String(index + 1), label: String(index + 1) }))} />{batchState && ["queued", "running"].includes(batchState.status) ? <button className="icon-button" aria-label="Pause project rendering" title="Pause project rendering" type="button" disabled={batchBusy} onClick={() => void pauseProjectBatch()}><Pause aria-hidden="true" size={13} /></button> : null}{batchState && ["paused", "failed"].includes(batchState.status) ? <button className="button button-secondary" type="button" disabled={batchBusy} onClick={() => void resumeProjectBatch()}><RotateCcw aria-hidden="true" size={13} />{batchState.status === "failed" ? "Retry failed" : "Resume"}</button> : null}{batchActive ? <button className="icon-button danger-button" aria-label="Cancel project rendering" title="Cancel project rendering" type="button" disabled={batchBusy} onClick={() => void cancelProjectBatch()}><CircleStop aria-hidden="true" size={13} /></button> : null}<button className="button button-primary" type="button" disabled={!staleChapters.length || Boolean(batchActive) || batchBusy} onClick={() => void renderStaleChapters()}>{batchBusy ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <Layers3 aria-hidden="true" size={13} />}{batchBusy ? "Updating" : `Render changed${staleChapters.length ? ` (${staleChapters.length})` : ""}`}</button></section>
            <section className="project-master-section"><div><strong>Master export</strong><small>Join rendered chapters into one delivery file.</small></div><SelectField label="Format" value={masterSettings.format} onChange={(format) => setMasterSettings((current) => ({ ...current, format: format as "wav" | "flac" }))} options={[{ value: "wav", label: "WAV" }, { value: "flac", label: "FLAC" }]} /><SelectField label="Rate" value={String(masterSettings.sample_rate)} onChange={(sample_rate) => setMasterSettings((current) => ({ ...current, sample_rate: Number(sample_rate) as ProjectMasterSettings["sample_rate"] }))} options={[{ value: "24000", label: "24 kHz" }, { value: "44100", label: "44.1 kHz" }, { value: "48000", label: "48 kHz" }]} /><label className="form-field"><span>Gap</span><input type="number" min="0" max="5000" step="50" value={masterSettings.gap_ms} onChange={(event) => setMasterSettings((current) => ({ ...current, gap_ms: Number(event.target.value) }))} /></label><label className="form-field"><span>LUFS</span><input type="number" min="-24" max="-9" step="1" value={masterSettings.target_lufs} onChange={(event) => setMasterSettings((current) => ({ ...current, target_lufs: Number(event.target.value) }))} /></label><button className="button button-primary" title={canMaster ? "Export mastered project" : "Render every written chapter before mastering"} type="button" disabled={!canMaster || mastering} onClick={() => void masterProject()}>{mastering ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <Download aria-hidden="true" size={13} />}{mastering ? "Mastering" : "Export master"}</button></section>
            <footer className="project-management"><StatusText tone="muted">{state?.startsWith("Master exported") ? state : `${chapters.length} chapters · ${chapters.filter((chapter) => chapter.history_id).length} rendered · ${chapters.filter((chapter) => chapter.text.trim() && !chapter.history_id).length} changed`}</StatusText><button className="text-button danger-button" type="button" disabled={!selected} onClick={() => void removeProject()}><Trash2 aria-hidden="true" size={13} />Delete project</button></footer>
          </div>
        </details>
      </Panel>
    </div> : <div className="page projects-page">
      <PageHeader title="Projects" subtitle="Every local production in one place. Open a row to continue it in its own workspace." actions={<button className="button button-primary" type="button" onClick={() => setProjectDialogMode("create")}><FolderPlus aria-hidden="true" size={14} />New project</button>} />
      <Panel className="table-panel" ariaLabel="All projects">
        <div className="project-table-controls">
          <label className="project-table-search">
            <Search aria-hidden="true" size={13} />
            <input aria-label="Filter projects by name" placeholder="Filter projects" value={search} onChange={(event) => setSearch(event.target.value)} />
          </label>
          <div className="project-table-filters" role="radiogroup" aria-label="Project type">
            {(["all", "video", "audio"] as const).map((value) => <button key={value} type="button" role="radio" aria-checked={kindFilter === value} className={kindFilter === value ? "is-active" : ""} onClick={() => setKindFilter(value)}>{value === "all" ? "All" : value === "video" ? "Video" : "Audio"}</button>)}
          </div>
          <span className="project-table-count">{filteredRows.length} of {allRows.length}</span>
        </div>
        {videoProjectsLoading && !allRows.length ? <div className="video-library-loading" role="status"><LoaderCircle className="spin" aria-hidden="true" size={14} />Loading projects</div>
          : filteredRows.length ? <table className="project-table">
            <thead><tr><th scope="col">Project</th><th scope="col">Type</th><th scope="col">Contents</th><th scope="col">Status</th><th scope="col">Updated</th></tr></thead>
            <tbody>
              {filteredRows.map((row) => <tr
                key={`${row.kind}-${row.id}`}
                className={row.kind === "audio" && row.id === selectedId ? "is-selected" : ""}
                // The whole row opens the project, because the row is the project.
                tabIndex={0}
                role="button"
                aria-label={`Open ${row.name}`}
                onClick={row.open}
                onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); row.open(); } }}
              >
                <td><strong>{row.name}</strong></td>
                <td><span className={`project-kind is-${row.kind}`}>{row.kind === "video" ? <Clapperboard aria-hidden="true" size={12} /> : <AudioLines aria-hidden="true" size={12} />}{row.kind === "video" ? "Video" : "Audio"}</span></td>
                <td>{row.detail}</td>
                <td>{row.status}</td>
                <td>{new Date(row.updatedAt).toLocaleDateString()}</td>
              </tr>)}
            </tbody>
          </table>
          : <EmptyState title={allRows.length ? "No projects match this filter" : "No projects yet"} detail={allRows.length ? "Clear the filter to see every local production." : videoProjectsError ?? "Use New project in the top-right, or start a video in Video Studio."} />}
      </Panel>
    </div>}
    {projectDialogMode ? <ProjectSetupDialog mode={projectDialogMode} initialName={projectDialogMode === "rename" ? name : ""} onClose={() => setProjectDialogMode(undefined)} onImport={projectDialogMode === "create" ? () => { setProjectDialogMode(undefined); void importScript(); } : undefined} onSubmit={(nextName) => { if (projectDialogMode === "create") createNew(nextName); else { setName(nextName); setState("Unsaved changes"); setProjectDialogMode(undefined); } }} /> : null}
    {chapterDialogOpen ? <ChapterSetupDialog position={chapters.length} onClose={() => setChapterDialogOpen(false)} onSubmit={addChapter} /> : null}
  </>;
}
