import { LoaderCircle, ScanText } from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { CandidateClipReview } from "../components/video/CandidateClipReview";
import { VideoAnalysisProgress } from "../components/video/VideoAnalysisProgress";
import { VideoEditorWorkspace } from "../components/video/VideoEditorWorkspace";
import { VideoExportComplete } from "../components/video/VideoExportComplete";
import { VideoIntakeDialog } from "../components/video/VideoIntakeDialog";
import { VideoProductionSteps } from "../components/video/VideoProductionSteps";
import { VideoStudioHome } from "../components/video/VideoStudioHome";
import { createVideoStudioService } from "../lib/videoBridge";
import { initialVideoStudioState, videoProjectReadiness, videoStudioReducer } from "../lib/videoState";
import type { BootstrapState, VoiceProfile } from "../types";
import type {
  AddVisualAssetRequest,
  CreateVideoProjectRequest,
  ImportLinkRequest,
  ImportLocalVideoRequest,
  VideoArtifact,
  VideoLinkPreview,
  VideoProject,
  VideoScene,
  VideoStudioService,
  VideoTimelineOperation,
  VideoTimelineEditResponse,
  VideoToolStatus,
  VideoVisualLayer,
} from "../types/video";
import "../video-studio.css";

export function VideoStudioView({
  service,
  initialProjectId,
  assistantOpen = false,
  onProjectChanged,
  onMasterPublished,
  bootstrap,
  voices = [],
}: {
  service?: VideoStudioService;
  initialProjectId?: string;
  assistantOpen?: boolean;
  onProjectChanged?: (project: VideoProject) => void;
  onMasterPublished?: (artifact: VideoArtifact, project: VideoProject) => void;
  bootstrap?: BootstrapState;
  voices?: VoiceProfile[];
}) {
  const serviceRef = useRef<VideoStudioService | null>(null);
  if (serviceRef.current === null) serviceRef.current = service ?? createVideoStudioService();
  const videoService = serviceRef.current;
  const [state, dispatch] = useReducer(videoStudioReducer, initialVideoStudioState);
  const [loadingProjects, setLoadingProjects] = useState(true);
  const [tools, setTools] = useState<VideoToolStatus[]>([]);
  const [statusAnnouncement, setStatusAnnouncement] = useState("Video Studio ready");
  const [timelineEditing, setTimelineEditing] = useState(false);
  const [visualAdding, setVisualAdding] = useState(false);
  const [timelineFeedback, setTimelineFeedback] = useState<{ tone: "status" | "error"; text: string }>();
  const [timelineUndo, setTimelineUndo] = useState<TimelineHistoryEntry[]>([]);
  const [timelineRedo, setTimelineRedo] = useState<TimelineHistoryEntry[]>([]);
  const operationGeneration = useRef(0);
  const activeJobId = useRef<string | undefined>(undefined);
  const pageRef = useRef<HTMLElement>(null);
  const previousPhase = useRef(state.phase);
  const timelineKnownVersion = useRef<string | undefined>(undefined);
  const timelineEditingRef = useRef(false);

  const refreshProjects = useCallback(async () => {
    const projects = await videoService.listVideoProjects();
    dispatch({ type: "projects-loaded", projects });
  }, [videoService]);

  useEffect(() => {
    let active = true;
    void Promise.all([videoService.listVideoProjects(), videoService.getToolStatus()]).then(([projects, statuses]) => {
      if (!active) return;
      dispatch({ type: "projects-loaded", projects });
      setTools(statuses);
    }).catch((caught) => {
      if (active) dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }).finally(() => {
      if (active) setLoadingProjects(false);
    });
    return () => { active = false; };
  }, [videoService]);

  useEffect(() => {
    if (!initialProjectId) return;
    let active = true;
    void videoService.getVideoProject(initialProjectId).then((project) => {
      if (active) dispatch({ type: "open-project", project });
    }).catch((caught) => {
      if (active) dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    });
    return () => { active = false; };
  }, [initialProjectId, videoService]);

  useEffect(() => {
    const changed = previousPhase.current !== state.phase;
    previousPhase.current = state.phase;
    if (!changed || !["analyzing", "review", "editor", "exported", "error"].includes(state.phase)) return;
    const frame = window.requestAnimationFrame(() => {
      const heading = pageRef.current?.querySelector<HTMLElement>("h1, h2");
      if (!heading) return;
      heading.tabIndex = -1;
      heading.focus({ preventScroll: true });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [state.phase]);

  useEffect(() => {
    const currentVersion = state.project?.manifest.version_id;
    if (!currentVersion) {
      timelineKnownVersion.current = undefined;
      setTimelineUndo([]);
      setTimelineRedo([]);
      return;
    }
    if (timelineKnownVersion.current && timelineKnownVersion.current !== currentVersion) {
      setTimelineUndo([]);
      setTimelineRedo([]);
      setTimelineFeedback(undefined);
    }
    timelineKnownVersion.current = currentVersion;
  }, [state.project?.manifest.version_id]);

  const previewLink = useCallback((exactUrl: string): Promise<VideoLinkPreview> => videoService.previewLink(exactUrl), [videoService]);
  const pickLocalVideo = useMemo(() => videoService.pickLocalVideo ? () => videoService.pickLocalVideo!() : undefined, [videoService]);
  const pickLocalAudio = useMemo(() => videoService.pickLocalAudio ? () => videoService.pickLocalAudio!() : undefined, [videoService]);

  async function analyze(
    project: VideoProject,
    returnPhase: "home" | "editor" = state.project?.id === project.id ? "editor" : "home",
  ) {
    const operation = ++operationGeneration.current;
    dispatch({ type: "source-accepted", project, returnPhase });
    void refreshProjects().catch(() => undefined);
    setStatusAnnouncement(`Analyzing ${project.manifest.source.display_name}.`);
    try {
      const analyzed = await videoService.analyzeVideo(project.id, (update) => {
        if (operation !== operationGeneration.current) return;
        activeJobId.current = update.job.id;
        dispatch({ type: "progress", job: update.job });
        setStatusAnnouncement(`${update.job.title}: ${Math.round(update.job.progress * 100)} percent`);
      });
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "analysis-complete", project: analyzed });
      onProjectChanged?.(analyzed);
      setStatusAnnouncement("Analysis complete. Candidate clips are ready for review.");
      await refreshProjects();
    } catch (caught) {
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function continueFromSource(project: VideoProject) {
    if (videoProjectReadiness(project).nextAction === "analyze") {
      await analyze(project);
      return;
    }
    dispatch({ type: "open-project", project });
    onProjectChanged?.(project);
    setStatusAnnouncement(`${project.name} is ready for the next production step.`);
    await refreshProjects();
  }

  async function resumeWorkflow(project: VideoProject) {
    const recoverable = project.recoverable_job;
    if (!recoverable) {
      dispatch({ type: "fail", error: "This project has no durable Video Studio task to resume." });
      return;
    }
    const operation = ++operationGeneration.current;
    try {
      const queued = await videoService.resumeVideoJob(recoverable.id);
      activeJobId.current = queued.id;
      dispatch({ type: "source-accepted", project: { ...project, workflow_job: queued, recoverable_job: undefined }, job: queued });
      setStatusAnnouncement("Durable Video Studio task resumed.");
      const deadline = Date.now() + 6 * 60 * 60 * 1_000;
      while (operation === operationGeneration.current && Date.now() < deadline) {
        await new Promise((resolve) => window.setTimeout(resolve, 350));
        const refreshed = await videoService.getVideoProject(project.id);
        if (operation !== operationGeneration.current) return;
        const workflow = refreshed.workflow_job;
        if (workflow && ["queued", "preparing", "running"].includes(workflow.status)) {
          activeJobId.current = workflow.id;
          dispatch({ type: "progress", job: workflow });
          setStatusAnnouncement(`${workflow.title}: ${Math.round(workflow.progress * 100)} percent`);
          continue;
        }
        if (refreshed.recoverable_job) {
          throw new Error(refreshed.recoverable_job.error ?? "The resumed Video Studio task needs attention.");
        }
        if (!workflow || !["queued", "preparing", "running"].includes(workflow.status)) {
          activeJobId.current = undefined;
          dispatch({ type: "open-project", project: refreshed });
          onProjectChanged?.(refreshed);
          setStatusAnnouncement("The resumed Video Studio task completed.");
          await refreshProjects();
          return;
        }
      }
      throw new Error("The resumed Video Studio task is still running; reopen the project to keep monitoring it.");
    } catch (caught) {
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function importLink(request: ImportLinkRequest) {
    await continueFromSource(await videoService.importLink(request));
  }

  async function importLocalVideo(request: ImportLocalVideoRequest) {
    await continueFromSource(await videoService.importLocalVideo(request));
  }

  async function createFromPrompt(request: CreateVideoProjectRequest) {
    await continueFromSource(await videoService.createVideoProject(request));
  }

  async function openProject(projectId: string) {
    try {
      const project = await videoService.getVideoProject(projectId);
      dispatch({ type: "open-project", project });
      setStatusAnnouncement(`${project.name} opened.`);
    } catch (caught) {
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function completeReview() {
    if (!state.project || !state.selectedCandidateIds.length) return;
    try {
      const project = await videoService.planVideo(state.project.id, state.selectedCandidateIds);
      dispatch({ type: "review-complete", project });
      onProjectChanged?.(project);
      setStatusAnnouncement("Scene plan added to the timeline.");
      await refreshProjects();
    } catch (caught) {
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function renderPreview() {
    if (!state.project) return;
    const operation = ++operationGeneration.current;
    dispatch({ type: "render-started" });
    try {
      const project = await videoService.renderVideoPreview(state.project.id, (update) => {
        if (operation !== operationGeneration.current) return;
        activeJobId.current = update.job.id;
        dispatch({ type: "progress", job: update.job });
        setStatusAnnouncement(`${update.job.title}: ${Math.round(update.job.progress * 100)} percent`);
      });
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "preview-complete", project });
      onProjectChanged?.(project);
      setStatusAnnouncement("Preview render is playable.");
    } catch (caught) {
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function exportVideo() {
    if (!state.project) return;
    const operation = ++operationGeneration.current;
    dispatch({ type: "export-started" });
    try {
      const project = await videoService.exportVideo({ project_id: state.project.id, version_id: state.project.manifest.version_id, format: "mp4", profile: "final" }, (update) => {
        if (operation !== operationGeneration.current) return;
        activeJobId.current = update.job.id;
        dispatch({ type: "progress", job: update.job });
        setStatusAnnouncement(`${update.job.title}: ${Math.round(update.job.progress * 100)} percent`);
      });
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "export-complete", project });
      onProjectChanged?.(project);
      if (project.master) onMasterPublished?.(project.master, project);
      setStatusAnnouncement("Export complete. The final MP4 is playable and ready to download.");
      await refreshProjects();
    } catch (caught) {
      if (operation !== operationGeneration.current) return;
      activeJobId.current = undefined;
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function cancelOperation() {
    operationGeneration.current += 1;
    const jobId = activeJobId.current ?? state.activeJob?.id;
    activeJobId.current = undefined;
    dispatch({ type: "cancel-operation" });
    setStatusAnnouncement("Video operation cancelled. The project remains recoverable.");
    if (jobId) await videoService.cancelVideoJob(jobId).catch(() => false);
  }

  async function saveScene(scene: VideoScene) {
    if (!state.project) return;
    try {
      const savedScene = state.project.manifest.scenes.find((candidate) => candidate.id === scene.id);
      const narrationRouteChanged = !savedScene || (["voice_id", "model_id", "speaker", "language"] as const)
        .some((field) => savedScene[field] !== scene[field]);
      const project = await videoService.reviseVideo({
        project_id: state.project.id,
        base_version_id: state.project.manifest.version_id,
        instruction: `Update scene ${scene.position}: layout, crop, captions, audio mix, and narration route.`,
        scene_id: scene.id,
        scene_patch: {
          layout: scene.layout,
          crop_mode: scene.crop_mode,
          crop_rect: scene.crop_mode === "manual" ? scene.crop_rect : undefined,
          captions_enabled: scene.captions_enabled,
          caption_style: scene.caption_style,
          caption_bounds: scene.caption_bounds,
          voice_gain_db: scene.voice_gain_db,
          music_gain_db: scene.music_gain_db,
          ...(narrationRouteChanged ? {
            voice_id: scene.voice_id,
            model_id: scene.model_id,
            speaker: scene.speaker,
            language: scene.language,
          } : {}),
        },
      });
      dispatch({ type: "open-project", project });
      onProjectChanged?.(project);
      setStatusAnnouncement(`Saved ${scene.title}. Only affected render stages were invalidated.`);
      await refreshProjects();
    } catch (caught) {
      dispatch({ type: "fail", error: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  async function addVisual(sceneId?: string): Promise<boolean> {
    const project = state.project;
    if (!project) return false;
    setVisualAdding(true);
    try {
      const receipt = await videoService.chooseVideoVisualAsset({
        project_id: project.id,
        expected_revision: project.revision,
        expected_version_id: project.manifest.version_id,
      });
      if (!receipt) {
        setStatusAnnouncement("Image selection cancelled.");
        return false;
      }
      if (receipt.receipt_kind !== "user_selected"
        || receipt.project_id !== project.id
        || receipt.expected_revision !== project.revision
        || receipt.expected_version_id !== project.manifest.version_id) {
        throw new Error("video.invalid_visual_receipt: The image selection does not match this project version.");
      }
      const scene = sceneId ? project.manifest.scenes.find((candidate) => candidate.id === sceneId) : undefined;
      if (sceneId && !scene) throw new Error("video.missing_reference: The selected scene is no longer available.");
      const startUs = timelineMicroseconds(scene?.timeline_start_ms ?? 0);
      const endUs = timelineMicroseconds(scene?.timeline_end_ms ?? project.duration_ms);
      if (endUs <= startUs) throw new Error("video.invalid_timestamp: Choose a scene with a non-empty timeline range.");
      const transitionUs = Math.min(300_000, Math.floor((endUs - startUs) / 4));
      const fullCanvas = { x_bp: 0, y_bp: 0, width_bp: 10_000, height_bp: 10_000 };
      const highestLayer = Math.max(9, ...(project.manifest.visual_layers ?? []).map((layer) => layer.z_index));
      const request: AddVisualAssetRequest = {
        project_id: project.id,
        expected_revision: project.revision,
        expected_version_id: project.manifest.version_id,
        operation_id: newVideoOperationId("visual"),
        actor: "desktop-ui",
        // Only the native backend chooser can mint this one-use receipt.
        origin: { kind: "user_selected", receipt_id: receipt.id },
        scene_id: scene?.id,
        range: { start_us: startUs, end_us: endUs },
        fit: "contain",
        z_index: Math.min(32_767, highestLayer + 1),
        motion: {
          start_bounds: fullCanvas,
          end_bounds: fullCanvas,
          start_opacity_milli: 1_000,
          end_opacity_milli: 1_000,
          start_rotation_milli_degrees: 0,
          end_rotation_milli_degrees: 0,
          easing: "ease_in_out",
        },
        transition_in_us: transitionUs,
        transition_out_us: transitionUs,
      };
      setTimelineFeedback({ tone: "status", text: `Adding ${receipt.display_name} to the saved scene…` });
      const response = await videoService.addVideoVisualAsset(request);
      timelineKnownVersion.current = response.project.manifest.version_id;
      dispatch({ type: "open-project", project: response.project });
      const revealMs = Math.min(endUs / 1_000, Math.max(state.playheadMs, startUs / 1_000 + transitionUs / 1_000));
      dispatch({ type: "set-playhead", playheadMs: revealMs });
      onProjectChanged?.(response.project);
      setTimelineUndo([]);
      setTimelineRedo([]);
      setTimelineFeedback({ tone: "status", text: `${receipt.display_name} added. Preview and export will refresh.` });
      setStatusAnnouncement(`${receipt.display_name} was added to scene ${scene?.position ?? "timeline"}.`);
      void refreshProjects().catch(() => undefined);
      return true;
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught);
      setTimelineFeedback({ tone: "error", text: `The image was not added. ${message}` });
      setStatusAnnouncement("The image was not added. The project remains unchanged.");
      return false;
    } finally {
      setVisualAdding(false);
    }
  }

  async function executeTimelineEdit(project: VideoProject, operations: VideoTimelineOperation[], label: string): Promise<VideoTimelineEditResponse | undefined> {
    if (timelineEditingRef.current) return undefined;
    timelineEditingRef.current = true;
    setTimelineEditing(true);
    setTimelineFeedback({ tone: "status", text: `${label}…` });
    try {
      const response = await videoService.editVideoTimeline({
        project_id: project.id,
        expected_revision: project.revision,
        base_version_id: project.manifest.version_id,
        operation_id: newTimelineOperationId(),
        operations,
      });
      timelineKnownVersion.current = response.project.manifest.version_id;
      dispatch({ type: "open-project", project: response.project });
      onProjectChanged?.(response.project);
      setTimelineFeedback({ tone: "status", text: `${label} saved. ${response.receipt.invalidated_stages.length} dependent stages will refresh.` });
      setStatusAnnouncement(`${label} saved to revision ${response.project.revision}.`);
      void refreshProjects().catch(() => undefined);
      return response;
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught);
      setTimelineFeedback({ tone: "error", text: `${label} was not saved. The timeline was restored. ${message}` });
      setStatusAnnouncement(`${label} failed. The timeline was restored.`);
      return undefined;
    } finally {
      timelineEditingRef.current = false;
      setTimelineEditing(false);
    }
  }

  async function editTimeline(operations: VideoTimelineOperation[], label: string) {
    if (!state.project) return;
    const before = state.project;
    const response = await executeTimelineEdit(before, operations, label);
    if (!response) return;
    const inverse = deriveImmediateInverse(before, response.project, operations);
    setTimelineRedo([]);
    if (inverse) {
      setTimelineUndo((history) => [...history.slice(-19), {
        label,
        forward: operations,
        inverse,
        expected_revision: response.project.revision,
        expected_version_id: response.project.manifest.version_id,
      }]);
    } else {
      setTimelineUndo([]);
      setTimelineFeedback({ tone: "status", text: `${label} saved. Restore destructive trims from project revision history.` });
    }
  }

  async function undoTimelineEdit() {
    const project = state.project;
    const entry = timelineUndo.at(-1);
    if (!project || !entry || timelineEditing) return;
    if (project.revision !== entry.expected_revision || project.manifest.version_id !== entry.expected_version_id) {
      setTimelineUndo([]);
      setTimelineRedo([]);
      setTimelineFeedback({ tone: "error", text: "Undo was cleared because the project changed in another workflow." });
      return;
    }
    const response = await executeTimelineEdit(project, entry.inverse, `Undo ${entry.label.toLowerCase()}`);
    if (!response) return;
    setTimelineUndo((history) => history.slice(0, -1));
    setTimelineRedo((history) => [...history.slice(-19), { ...entry, expected_revision: response.project.revision, expected_version_id: response.project.manifest.version_id }]);
  }

  async function redoTimelineEdit() {
    const project = state.project;
    const entry = timelineRedo.at(-1);
    if (!project || !entry || timelineEditing) return;
    if (project.revision !== entry.expected_revision || project.manifest.version_id !== entry.expected_version_id) {
      setTimelineUndo([]);
      setTimelineRedo([]);
      setTimelineFeedback({ tone: "error", text: "Redo was cleared because the project changed in another workflow." });
      return;
    }
    const response = await executeTimelineEdit(project, entry.forward, `Redo ${entry.label.toLowerCase()}`);
    if (!response) return;
    setTimelineRedo((history) => history.slice(0, -1));
    setTimelineUndo((history) => [...history.slice(-19), { ...entry, expected_revision: response.project.revision, expected_version_id: response.project.manifest.version_id }]);
  }

  function selectScene(sceneId: string) {
    dispatch({ type: "select-scene", sceneId });
    const scene = state.project?.manifest.scenes.find((candidate) => candidate.id === sceneId);
    if (scene) dispatch({ type: "set-playhead", playheadMs: scene.timeline_start_ms });
  }

  async function publishPackage(): Promise<VideoArtifact> {
    if (!state.project) throw new Error("No video project is open.");
    const artifact = await videoService.exportPublishPackage(state.project.id);
    setStatusAnnouncement("Publish package is ready.");
    return artifact;
  }

  const pageClass = `page video-studio-page ${assistantOpen ? "is-assistant-companion" : ""} is-phase-${state.phase}`;
  const showHome = state.phase === "home" || state.phase === "intake";
  const needsAnalysis = state.phase === "editor"
    && Boolean(state.project)
    && videoProjectReadiness(state.project!).nextAction === "analyze";
  return (
    <section ref={pageRef} className={pageClass} aria-label="Video Studio">
      <span className="video-visually-hidden" role="status" aria-live="polite" aria-atomic="true">{statusAnnouncement}</span>
      {showHome ? <>
        <header className="video-studio-header"><div><h1>Video Studio</h1><p>Build, review, and render local video projects.</p></div><div><button className="video-button is-primary" type="button" disabled={!state.projects.length} onClick={() => state.projects[0] && void openProject(state.projects[0].id)}>Open latest project</button></div></header>
        <VideoStudioHome projects={state.projects} loading={loadingProjects} onEntry={(entry) => dispatch({ type: "open-intake", entry })} onOpenProject={(projectId) => void openProject(projectId)} />
        <footer className="video-tool-footer"><span>Media tools</span>{tools.slice(0, 4).map((tool) => <span key={tool.id} className={`is-${tool.state}`}><i aria-hidden="true" />{tool.label} {tool.state === "ready" ? "Ready" : "Setup needed"}</span>)}</footer>
      </> : null}

      {state.phase === "intake" && state.entry ? <VideoIntakeDialog entry={state.entry} tools={tools} onClose={() => dispatch({ type: "close-intake" })} onPreviewLink={previewLink} onPickLocalVideo={pickLocalVideo} onPickLocalAudio={pickLocalAudio} onImportLink={importLink} onImportLocalVideo={importLocalVideo} onCreateVideo={createFromPrompt} /> : null}
      {state.phase === "analyzing" && state.project ? <VideoAnalysisProgress project={state.project} job={state.activeJob} onCancel={() => void cancelOperation()} onResume={state.project.recoverable_job ? () => void resumeWorkflow(state.project!) : undefined} /> : null}
      {state.phase === "review" && state.project ? <CandidateClipReview project={state.project} selectedIds={state.selectedCandidateIds} onToggle={(candidateId) => dispatch({ type: "toggle-candidate", candidateId })} onContinue={() => void completeReview()} onBack={() => dispatch({ type: "reset" })} /> : null}
      {needsAnalysis && state.project ? <section className="video-preparation-state" aria-labelledby="video-preparation-title">
        <header className="video-editor-toolbar"><div><span>Video Studio</span><h2 title={state.project.name}>{state.project.name}</h2></div></header>
        <VideoProductionSteps project={state.project} />
        <div className="video-preparation-card">
          <span className="video-preparation-icon"><ScanText aria-hidden="true" size={18} /></span>
          <div><h2 id="video-preparation-title">Analyze this source</h2><p>Transcribe the source clock, preserve its pauses, and prepare candidate moments before editing.</p><small>{state.project.manifest.source.display_name}</small></div>
          <button className="video-button is-primary" type="button" onClick={() => void analyze(state.project!, "editor")}>Analyze source</button>
        </div>
      </section> : null}
      {["editor", "rendering", "exporting"].includes(state.phase) && state.project && !needsAnalysis ? <VideoEditorWorkspace project={state.project} selectedSceneId={state.selectedSceneId} playheadMs={state.playheadMs} job={state.activeJob} working={state.phase === "rendering" ? "rendering" : state.phase === "exporting" ? "exporting" : undefined} onSelectScene={selectScene} onPlayheadChange={(playheadMs) => dispatch({ type: "set-playhead", playheadMs })} onRenderPreview={() => void renderPreview()} onExport={() => void exportVideo()} onSaveScene={saveScene} onAddVisual={addVisual} visualAdding={visualAdding} onEditTimeline={editTimeline} timelineEditing={timelineEditing} timelineFeedback={timelineFeedback} canUndoTimeline={Boolean(timelineUndo.length && timelineUndo.at(-1)?.expected_version_id === state.project.manifest.version_id)} canRedoTimeline={Boolean(timelineRedo.length && timelineRedo.at(-1)?.expected_version_id === state.project.manifest.version_id)} onUndoTimeline={() => void undoTimelineEdit()} onRedoTimeline={() => void redoTimelineEdit()} onCancelWorking={() => void cancelOperation()} bootstrap={bootstrap} voices={voices} /> : null}
      {state.phase === "exported" && state.project ? <VideoExportComplete project={state.project} playheadMs={state.playheadMs} selectedSceneId={state.selectedSceneId} onPlayheadChange={(playheadMs) => dispatch({ type: "set-playhead", playheadMs })} onSelectScene={selectScene} onEdit={() => dispatch({ type: "preview-complete", project: state.project! })} onPublishPackage={publishPackage} /> : null}
      {state.phase === "error" ? <div className="video-error-state" role="alert"><h2>Video Studio needs attention</h2><p>{state.error}</p><button className="video-button is-primary" type="button" onClick={() => dispatch({ type: "dismiss-error" })}>Return to project</button></div> : null}
      {loadingProjects && !showHome ? <div className="video-corner-loading" role="status"><LoaderCircle className="video-spin" aria-hidden="true" size={14} />Refreshing projects</div> : null}
    </section>
  );
}

export default VideoStudioView;

interface TimelineHistoryEntry {
  label: string;
  forward: VideoTimelineOperation[];
  inverse: VideoTimelineOperation[];
  expected_revision: number;
  expected_version_id: string;
}

function newVideoOperationId(prefix: "timeline" | "visual"): string {
  const random = typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : Math.random().toString(36).slice(2);
  return `${prefix}-${Date.now().toString(36)}-${random}`;
}

function newTimelineOperationId(): string {
  return newVideoOperationId("timeline");
}

function timelineMicroseconds(milliseconds: number): number {
  const value = Math.round(milliseconds * 1_000);
  if (!Number.isSafeInteger(value) || value < 0) throw new Error("video.invalid_timestamp: Timeline time cannot be represented exactly in microseconds.");
  return value;
}

function deriveImmediateInverse(before: VideoProject, after: VideoProject, operations: VideoTimelineOperation[]): VideoTimelineOperation[] | undefined {
  if (operations.length !== 1) return undefined;
  const operation = operations[0];
  if (operation.type === "split_scene") {
    const firstIndex = after.manifest.scenes.findIndex((scene) => scene.id === operation.scene_id);
    const second = after.manifest.scenes[firstIndex + 1];
    return firstIndex >= 0 && second
      ? [{ type: "merge_scenes", first_scene_id: operation.scene_id, second_scene_id: second.id }]
      : undefined;
  }
  if (operation.type === "reorder_scene") {
    const originalIndex = before.manifest.scenes.findIndex((scene) => scene.id === operation.scene_id);
    return originalIndex >= 0 ? [{ type: "reorder_scene", scene_id: operation.scene_id, to_index: originalIndex }] : undefined;
  }
  if (operation.type === "merge_scenes") {
    const first = before.manifest.scenes.find((scene) => scene.id === operation.first_scene_id);
    return first ? [{ type: "split_scene", scene_id: operation.first_scene_id, at_timeline_us: timelineMicroseconds(first.timeline_end_ms) }] : undefined;
  }
  if (operation.type === "update_visual_layer") {
    const layer = before.manifest.visual_layers?.find((candidate) => candidate.id === operation.layer_id);
    return layer ? [visualLayerUpdateOperation(layer)] : undefined;
  }
  return undefined;
}

function visualLayerUpdateOperation(layer: VideoVisualLayer): Extract<VideoTimelineOperation, { type: "update_visual_layer" }> {
  return {
    type: "update_visual_layer",
    layer_id: layer.id,
    scene_id: layer.scene_id ?? null,
    range: { start_us: timelineMicroseconds(layer.start_ms), end_us: timelineMicroseconds(layer.end_ms) },
    fit: layer.fit,
    crop: layer.crop ?? null,
    z_index: layer.z_index,
    motion: layer.motion,
    transition_in_us: timelineMicroseconds(layer.transition_in_ms),
    transition_out_us: timelineMicroseconds(layer.transition_out_ms),
  };
}
