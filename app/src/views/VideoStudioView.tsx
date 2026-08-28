import { LoaderCircle } from "lucide-react";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { CandidateClipReview } from "../components/video/CandidateClipReview";
import { VideoAnalysisProgress } from "../components/video/VideoAnalysisProgress";
import { VideoEditorWorkspace } from "../components/video/VideoEditorWorkspace";
import { VideoExportComplete } from "../components/video/VideoExportComplete";
import { VideoIntakeDialog } from "../components/video/VideoIntakeDialog";
import { VideoStudioHome } from "../components/video/VideoStudioHome";
import { createVideoStudioService } from "../lib/videoBridge";
import { initialVideoStudioState, videoStudioReducer } from "../lib/videoState";
import type { BootstrapState, VoiceProfile } from "../types";
import type {
  CreateVideoProjectRequest,
  ImportLinkRequest,
  ImportLocalVideoRequest,
  VideoArtifact,
  VideoLinkPreview,
  VideoProject,
  VideoScene,
  VideoStudioService,
  VideoToolStatus,
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
  const operationGeneration = useRef(0);
  const activeJobId = useRef<string | undefined>(undefined);
  const pageRef = useRef<HTMLElement>(null);
  const previousPhase = useRef(state.phase);

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

  const previewLink = useCallback((exactUrl: string): Promise<VideoLinkPreview> => videoService.previewLink(exactUrl), [videoService]);
  const pickLocalVideo = useMemo(() => videoService.pickLocalVideo ? () => videoService.pickLocalVideo!() : undefined, [videoService]);
  const pickLocalAudio = useMemo(() => videoService.pickLocalAudio ? () => videoService.pickLocalAudio!() : undefined, [videoService]);

  async function analyze(project: VideoProject) {
    const operation = ++operationGeneration.current;
    dispatch({ type: "source-accepted", project });
    void refreshProjects().catch(() => undefined);
    setStatusAnnouncement("Video source accepted. Analysis started.");
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
    await analyze(await videoService.importLink(request));
  }

  async function importLocalVideo(request: ImportLocalVideoRequest) {
    await analyze(await videoService.importLocalVideo(request));
  }

  async function createFromPrompt(request: CreateVideoProjectRequest) {
    const project = await videoService.createVideoProject(request);
    dispatch({ type: "open-project", project });
    onProjectChanged?.(project);
    setStatusAnnouncement("Video project created from the prompt or audio source.");
    await refreshProjects();
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
      {["editor", "rendering", "exporting"].includes(state.phase) && state.project ? <VideoEditorWorkspace project={state.project} selectedSceneId={state.selectedSceneId} playheadMs={state.playheadMs} job={state.activeJob} working={state.phase === "rendering" ? "rendering" : state.phase === "exporting" ? "exporting" : undefined} onSelectScene={selectScene} onPlayheadChange={(playheadMs) => dispatch({ type: "set-playhead", playheadMs })} onRenderPreview={() => void renderPreview()} onExport={() => void exportVideo()} onSaveScene={saveScene} onCancelWorking={() => void cancelOperation()} bootstrap={bootstrap} voices={voices} /> : null}
      {state.phase === "exported" && state.project ? <VideoExportComplete project={state.project} playheadMs={state.playheadMs} selectedSceneId={state.selectedSceneId} onPlayheadChange={(playheadMs) => dispatch({ type: "set-playhead", playheadMs })} onSelectScene={selectScene} onEdit={() => dispatch({ type: "preview-complete", project: state.project! })} onPublishPackage={publishPackage} /> : null}
      {state.phase === "error" ? <div className="video-error-state" role="alert"><h2>Video Studio needs attention</h2><p>{state.error}</p><button className="video-button is-primary" type="button" onClick={() => dispatch({ type: "dismiss-error" })}>Return to project</button></div> : null}
      {loadingProjects && !showHome ? <div className="video-corner-loading" role="status"><LoaderCircle className="video-spin" aria-hidden="true" size={14} />Refreshing projects</div> : null}
    </section>
  );
}

export default VideoStudioView;
