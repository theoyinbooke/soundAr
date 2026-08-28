import type {
  CandidateVideoClip,
  VideoArtifact,
  VideoJob,
  VideoProject,
  VideoProjectSummary,
  VideoStudioEntry,
  VideoStudioPhase,
} from "../types/video";

export interface VideoStudioState {
  phase: VideoStudioPhase;
  returnPhase: Exclude<VideoStudioPhase, "error">;
  entry?: VideoStudioEntry;
  projects: VideoProjectSummary[];
  project?: VideoProject;
  selectedCandidateIds: string[];
  selectedSceneId?: string;
  playheadMs: number;
  activeJob?: VideoJob;
  error?: string;
}

export function formatVideoUpdatedAt(timestamp: string, now = Date.now()): string {
  const value = new Date(timestamp).getTime();
  if (!Number.isFinite(value)) return "Unknown";
  const elapsed = Math.max(0, now - value);
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  if (elapsed < minute) return "Just now";
  if (elapsed < hour) return `${Math.floor(elapsed / minute)}m ago`;
  if (elapsed < day) return `${Math.floor(elapsed / hour)}h ago`;
  if (elapsed < 7 * day) return `${Math.floor(elapsed / day)}d ago`;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(value);
}

export type VideoStudioAction =
  | { type: "projects-loaded"; projects: VideoProjectSummary[] }
  | { type: "open-intake"; entry: VideoStudioEntry }
  | { type: "close-intake" }
  | { type: "source-accepted"; project: VideoProject; job?: VideoJob }
  | { type: "progress"; job: VideoJob }
  | { type: "analysis-complete"; project: VideoProject }
  | { type: "toggle-candidate"; candidateId: string }
  | { type: "review-complete"; project: VideoProject }
  | { type: "open-project"; project: VideoProject }
  | { type: "select-scene"; sceneId: string }
  | { type: "set-playhead"; playheadMs: number }
  | { type: "render-started"; job?: VideoJob }
  | { type: "preview-complete"; project: VideoProject }
  | { type: "export-started"; job?: VideoJob }
  | { type: "export-complete"; project: VideoProject }
  | { type: "cancel-operation" }
  | { type: "fail"; error: string }
  | { type: "dismiss-error" }
  | { type: "reset" };

export const initialVideoStudioState: VideoStudioState = {
  phase: "home",
  returnPhase: "home",
  projects: [],
  selectedCandidateIds: [],
  playheadMs: 0,
};

function selectedCandidates(project: VideoProject): string[] {
  return project.manifest.candidates.filter((candidate) => candidate.selected).map((candidate) => candidate.id);
}

function phaseForProject(project: VideoProject): VideoStudioPhase {
  if (project.master || project.status === "exported") return "exported";
  if (project.recoverable_job || (project.workflow_job && ["queued", "preparing", "running"].includes(project.workflow_job.status))) return "analyzing";
  if (project.status === "review") return "review";
  if (project.status === "analyzing") return "analyzing";
  return "editor";
}

function withProject(state: VideoStudioState, project: VideoProject, phase = phaseForProject(project)): VideoStudioState {
  return {
    ...state,
    phase,
    returnPhase: phase === "error" ? state.returnPhase : phase,
    entry: undefined,
    project,
    selectedCandidateIds: selectedCandidates(project),
    selectedSceneId: project.manifest.scenes.some((scene) => scene.id === state.selectedSceneId)
      ? state.selectedSceneId
      : project.manifest.scenes[0]?.id,
    playheadMs: Math.min(state.playheadMs, project.manifest.timeline.duration_ms),
    activeJob: undefined,
    error: undefined,
  };
}

export function videoStudioReducer(state: VideoStudioState, action: VideoStudioAction): VideoStudioState {
  switch (action.type) {
    case "projects-loaded":
      return { ...state, projects: action.projects };
    case "open-intake":
      return { ...state, phase: "intake", returnPhase: state.phase === "error" ? state.returnPhase : state.phase, entry: action.entry, error: undefined };
    case "close-intake":
      return { ...state, phase: state.project ? phaseForProject(state.project) : "home", entry: undefined, error: undefined };
    case "source-accepted":
      return {
        ...withProject(state, action.project, "analyzing"),
        returnPhase: "home",
        activeJob: action.job,
        selectedCandidateIds: [],
      };
    case "progress":
      return { ...state, activeJob: action.job };
    case "analysis-complete":
      return withProject(state, action.project, "review");
    case "toggle-candidate": {
      const exists = state.selectedCandidateIds.includes(action.candidateId);
      return {
        ...state,
        selectedCandidateIds: exists
          ? state.selectedCandidateIds.filter((id) => id !== action.candidateId)
          : [...state.selectedCandidateIds, action.candidateId],
      };
    }
    case "review-complete":
      return withProject(state, action.project, "editor");
    case "open-project":
      return withProject(state, action.project);
    case "select-scene":
      return { ...state, selectedSceneId: action.sceneId };
    case "set-playhead": {
      const duration = state.project?.manifest.timeline.duration_ms ?? 0;
      return { ...state, playheadMs: Math.max(0, Math.min(action.playheadMs, duration)) };
    }
    case "render-started":
      return { ...state, phase: "rendering", returnPhase: "editor", activeJob: action.job, error: undefined };
    case "preview-complete":
      return withProject(state, action.project, "editor");
    case "export-started":
      return { ...state, phase: "exporting", returnPhase: "editor", activeJob: action.job, error: undefined };
    case "export-complete":
      return withProject(state, action.project, "exported");
    case "cancel-operation":
      return { ...state, phase: state.returnPhase, activeJob: undefined, error: undefined };
    case "fail": {
      const safeReturnPhase = state.phase === "analyzing"
        ? "home"
        : state.phase === "rendering" || state.phase === "exporting"
          ? "editor"
          : state.phase === "error" ? state.returnPhase : state.phase;
      return { ...state, phase: "error", returnPhase: safeReturnPhase, activeJob: undefined, error: action.error };
    }
    case "dismiss-error":
      return { ...state, phase: state.returnPhase, error: undefined };
    case "reset":
      return { ...initialVideoStudioState, projects: state.projects };
  }
}

export function candidateDuration(candidate: CandidateVideoClip): number {
  return Math.max(0, candidate.source_end_ms - candidate.source_start_ms);
}

export function selectPreviewArtifact(project?: VideoProject): VideoArtifact | undefined {
  return [...(project?.manifest.artifacts ?? [])].reverse().find((artifact) => artifact.role === "preview" && artifact.playable);
}

export function selectMasterArtifact(project?: VideoProject): VideoArtifact | undefined {
  return project?.master ?? [...(project?.manifest.artifacts ?? [])].reverse().find((artifact) => artifact.role === "master" && artifact.playable);
}

export function formatVideoClock(milliseconds: number, includeHours = false): string {
  const totalSeconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours || includeHours
    ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}
