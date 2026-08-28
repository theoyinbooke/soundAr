export type VideoStudioEntry = "link" | "upload" | "prompt";

export type VideoStudioPhase =
  | "home"
  | "intake"
  | "analyzing"
  | "review"
  | "editor"
  | "rendering"
  | "exporting"
  | "exported"
  | "error";

export type VideoProjectStatus =
  | "draft"
  | "analyzing"
  | "review"
  | "editing"
  | "rendering"
  | "exported"
  | "failed";

export type VideoJobPhase = "source" | "analyze" | "review" | "preview" | "export";
export type VideoJobStatus = "queued" | "running" | "completed" | "failed" | "cancelled";
export type VideoTimelineTrackKind = "video" | "captions" | "voice" | "music";
export type VideoArtifactRole = "source" | "proxy" | "preview" | "master" | "variation" | "publish-package";

export interface VideoToolStatus {
  id: "ffmpeg" | "ffprobe" | "yt-dlp" | "javascript" | "transcriber";
  label: string;
  state: "ready" | "setup-needed" | "unavailable";
  detail?: string;
}

export interface VideoSourceAsset {
  id: string;
  kind: "link" | "local-video" | "audio" | "prompt";
  exact_url?: string;
  local_path?: string;
  display_name: string;
  duration_ms: number;
  width?: number;
  height?: number;
  mime_type?: string;
  rights_confirmed: boolean;
  rights_confirmed_at?: string;
  rights_confirmation_url?: string;
  preview_url?: string;
  poster_url?: string;
  provenance: string;
}

export interface VideoLinkPreview {
  exact_url: string;
  title: string;
  creator: string;
  duration_ms: number;
  published_label: string;
  view_label?: string;
  preview_url?: string;
  poster_url?: string;
  is_single_source: boolean;
}

export interface VideoTranscriptSegment {
  id: string;
  start_ms: number;
  end_ms: number;
  text: string;
  speaker?: string;
  source_clock: true;
}

export interface CandidateVideoClip {
  id: string;
  rank: number;
  source_start_ms: number;
  source_end_ms: number;
  title: string;
  transcript: string;
  score: number;
  selected: boolean;
  poster_url?: string;
}

export interface VideoScene {
  id: string;
  candidate_id?: string;
  position: number;
  title: string;
  source_start_ms: number;
  source_end_ms: number;
  timeline_start_ms: number;
  timeline_end_ms: number;
  transcript: string;
  layout: "portrait" | "landscape" | "square";
  crop_mode: "auto-center" | "fit" | "manual";
  captions_enabled: boolean;
  caption_style: "clean-white" | "calm" | "kinetic";
  voice_gain_db: number;
  music_gain_db: number;
}

export interface VideoTimelineItem {
  id: string;
  track: VideoTimelineTrackKind;
  kind: "clip" | "gap" | "bed";
  start_ms: number;
  end_ms: number;
  label: string;
  scene_id?: string;
  source_start_ms?: number;
  source_end_ms?: number;
}

export interface VideoTimelineManifest {
  duration_ms: number;
  source_clock_duration_ms: number;
  tracks: Array<{
    kind: VideoTimelineTrackKind;
    items: VideoTimelineItem[];
  }>;
}

export interface VideoArtifact {
  id: string;
  project_id: string;
  version_id: string;
  role: VideoArtifactRole;
  title: string;
  mime_type: string;
  format: string;
  url?: string;
  local_path?: string;
  download_name?: string;
  poster_url?: string;
  duration_ms?: number;
  width?: number;
  height?: number;
  frame_rate?: number;
  codec?: string;
  file_size_bytes?: number;
  checksum?: string;
  playable: boolean;
  created_at: string;
}

export interface VideoRevision {
  id: string;
  created_at: string;
  instruction: string;
  affected_stages: VideoJobPhase[];
  base_version_id: string;
  version_id: string;
}

export interface VideoProjectManifest {
  schema_version: 1;
  version_id: string;
  source: VideoSourceAsset;
  transcript_version: string;
  transcript: VideoTranscriptSegment[];
  candidates: CandidateVideoClip[];
  scenes: VideoScene[];
  timeline: VideoTimelineManifest;
  artifacts: VideoArtifact[];
  revisions: VideoRevision[];
  settings: {
    aspect_ratio: "9:16" | "16:9" | "1:1";
    caption_style: VideoScene["caption_style"];
    captions_enabled: boolean;
    hardware_render: boolean;
  };
}

export interface VideoProjectSummary {
  id: string;
  name: string;
  status: VideoProjectStatus;
  duration_ms: number;
  scene_count: number;
  updated_at: string;
  poster_url?: string;
  master?: VideoArtifact;
}

export interface VideoProject extends VideoProjectSummary {
  created_at: string;
  manifest: VideoProjectManifest;
}

export interface VideoJob {
  id: string;
  project_id: string;
  phase: VideoJobPhase;
  status: VideoJobStatus;
  progress: number;
  title: string;
  detail: string;
  durable: true;
  created_at: string;
  updated_at: string;
  error?: string;
}

export interface VideoProgressUpdate {
  job: VideoJob;
  partial_artifact?: VideoArtifact;
}

export interface ImportLinkRequest {
  exact_url: string;
  rights_confirmed: true;
  rights_confirmation_url: string;
  single_source_only: true;
}

export interface ImportLocalVideoRequest {
  file?: File;
  local_path?: string;
  display_name: string;
  rights_confirmed: true;
}

export interface LocalVideoSelection {
  file?: File;
  local_path?: string;
  display_name: string;
  size_bytes?: number;
}

export interface LocalAudioSelection {
  file?: File;
  local_path?: string;
  display_name: string;
  size_bytes?: number;
}

export interface CreateVideoProjectRequest {
  prompt: string;
  audio_file?: File;
  audio_local_path?: string;
  audio_display_name?: string;
  source_project_id?: string;
}

export interface ReviseVideoRequest {
  project_id: string;
  instruction: string;
  base_version_id: string;
  scene_id?: string;
  scene_patch?: Pick<VideoScene, "layout" | "crop_mode" | "captions_enabled" | "caption_style" | "voice_gain_db" | "music_gain_db">;
}

export interface VideoExportRequest {
  project_id: string;
  version_id: string;
  format: "mp4";
  profile: "preview" | "final";
  variations?: number;
}

export interface VideoStudioService {
  previewLink(exactUrl: string): Promise<VideoLinkPreview>;
  importLink(request: ImportLinkRequest): Promise<VideoProject>;
  pickLocalVideo?(): Promise<LocalVideoSelection | undefined>;
  pickLocalAudio?(): Promise<LocalAudioSelection | undefined>;
  importLocalVideo(request: ImportLocalVideoRequest): Promise<VideoProject>;
  analyzeVideo(projectId: string, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  planVideo(projectId: string, selectedCandidateIds?: string[]): Promise<VideoProject>;
  createVideoProject(request: CreateVideoProjectRequest): Promise<VideoProject>;
  listVideoProjects(): Promise<VideoProjectSummary[]>;
  getVideoProject(projectId: string): Promise<VideoProject>;
  renderVideoPreview(projectId: string, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  reviseVideo(request: ReviseVideoRequest): Promise<VideoProject>;
  exportVideo(request: VideoExportRequest, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  exportPublishPackage(projectId: string): Promise<VideoArtifact>;
  cancelVideoJob(jobId: string): Promise<boolean>;
  resumeVideoJob(jobId: string): Promise<VideoJob>;
  getToolStatus(): Promise<VideoToolStatus[]>;
}
