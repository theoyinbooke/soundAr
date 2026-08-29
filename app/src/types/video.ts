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
export type VideoJobStatus = "queued" | "preparing" | "running" | "completed" | "failed" | "cancelled";
export type VideoTimelineTrackKind = "video" | "visuals" | "captions" | "voice" | "music";
export type VideoArtifactRole = "source" | "proxy" | "preview" | "master" | "variation" | "publish-package";
export type VideoCaptionStyle =
  | "clean-white"
  | "calm"
  | "kinetic"
  | "bold-pop"
  | "highlight"
  | "karaoke"
  | "typewriter"
  | "podcast";

export interface VideoCanvasBounds {
  x_bp: number;
  y_bp: number;
  width_bp: number;
  height_bp: number;
}

export type VideoVisualFit = "cover" | "contain" | "stretch";
export type VideoVisualEasing = "linear" | "ease_in_out";

export interface VideoVisualProvenance {
  kind: "user_upload" | "authorized_link" | "existing_sound_ar_artifact" | "generated_locally";
  original_uri?: string | null;
  imported_at: string;
  producer: string;
  producer_version?: string | null;
  metadata: Record<string, unknown>;
}

export interface VideoVisualAsset {
  id: string;
  mime_type: "image/png" | "image/jpeg" | "image/webp";
  local_path: string;
  /** Desktop-only playback URL projected from the managed local path. */
  url?: string;
  width: number;
  height: number;
  has_alpha: boolean;
  size_bytes: number;
  checksum: string;
  provenance: VideoVisualProvenance;
  created_at: string;
}

export interface VideoVisualMotion {
  start_bounds: VideoCanvasBounds;
  end_bounds: VideoCanvasBounds;
  start_opacity_milli: number;
  end_opacity_milli: number;
  start_rotation_milli_degrees: number;
  end_rotation_milli_degrees: number;
  easing: VideoVisualEasing;
}

export interface VideoVisualLayer {
  id: string;
  asset_id: string;
  scene_id?: string | null;
  start_ms: number;
  end_ms: number;
  fit: VideoVisualFit;
  crop?: VideoCanvasBounds | null;
  z_index: number;
  motion: VideoVisualMotion;
  transition_in_ms: number;
  transition_out_ms: number;
}

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

/**
 * A caption design exactly as the renderer defines it.
 *
 * The editor renders both its preset chips and the live canvas caption from this, so the design a
 * user picks is the design FFmpeg burns into the export.
 */
export interface VideoCaptionPreset {
  id: VideoCaptionStyle;
  label: string;
  font_family: string;
  /** Font size as a fraction of canvas height. */
  relative_size: number;
  text_color: string;
  active_color: string;
  outline_color: string;
  /** Only opaque-box presets paint a background; the rest draw an outline instead. */
  background_color: string | null;
  bold: boolean;
  letter_spacing_em: number;
  outline_em: number;
  casing: "as-is" | "upper" | "lower";
  reveal: "page" | "active-word" | "karaoke" | "typewriter";
  max_words_per_page: number;
  max_lines: number;
}

export interface VideoCaptionWord {
  text: string;
  start_ms: number;
  end_ms: number;
}

export interface VideoCaptionPage {
  id: string;
  cue_id: string;
  scene_id?: string;
  start_ms: number;
  end_ms: number;
  text: string;
  style_id: VideoCaptionStyle;
  words: VideoCaptionWord[];
  bounds?: VideoCanvasBounds;
  font_size_bp?: number;
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
  crop_rect?: {
    x_bp: number;
    y_bp: number;
    width_bp: number;
    height_bp: number;
  };
  captions_enabled: boolean;
  caption_style: VideoCaptionStyle;
  caption_bounds?: VideoCanvasBounds;
  voice_gain_db: number;
  music_gain_db: number;
  narration_binding_id?: string;
  narration_history_id?: string;
  voice_id?: string;
  model_id?: string;
  speaker?: string;
  language?: string;
}

export interface VideoNarrationBinding {
  id: string;
  scene_id?: string;
  render_artifact_id: string;
  history_id: string;
  generation_job_id: string;
  voice_id: string;
  model_id: string;
  speaker: string;
  language: string;
  script_sha256: string;
  created_at: string;
}

export interface VideoTimelineItem {
  id: string;
  track: VideoTimelineTrackKind;
  kind: "clip" | "gap" | "bed";
  start_ms: number;
  end_ms: number;
  label: string;
  scene_id?: string | null;
  source_start_ms?: number;
  source_end_ms?: number;
  caption_style?: VideoCaptionStyle;
  bounds?: VideoCanvasBounds;
  font_size_bp?: number;
  asset_id?: string;
  start_bounds?: VideoCanvasBounds;
  end_bounds?: VideoCanvasBounds;
  fit?: VideoVisualFit;
  z_index?: number;
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
  /** Present on current manifests; optional only for migration-era project compatibility. */
  caption_pages?: VideoCaptionPage[];
  candidates: CandidateVideoClip[];
  scenes: VideoScene[];
  narration_bindings: VideoNarrationBinding[];
  /** Present on current manifests; optional only for migration-era project compatibility. */
  visual_assets?: VideoVisualAsset[];
  /** Present on current manifests; optional only for migration-era project compatibility. */
  visual_layers?: VideoVisualLayer[];
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
  revision: number;
  duration_ms: number;
  scene_count: number;
  updated_at: string;
  poster_url?: string;
  master?: VideoArtifact;
  deliverables?: VideoArtifact[];
}

export interface VideoProject extends VideoProjectSummary {
  created_at: string;
  manifest: VideoProjectManifest;
  workflow_job?: VideoJob;
  recoverable_job?: VideoJob;
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

export type VideoVisualAssetOrigin =
  | { kind: "user_selected"; receipt_id: string }
  | { kind: "generated_locally"; receipt_id: string };

export interface AuthorizeVisualSelectionRequest {
  project_id: string;
  expected_revision: number;
  expected_version_id: string;
}

export interface VisualSourceReceipt {
  id: string;
  receipt_kind: "user_selected" | "generated_locally";
  project_id: string;
  expected_revision: number;
  expected_version_id: string;
  display_name: string;
  sha256: string;
  mime_type: "image/png" | "image/jpeg" | "image/webp";
  size_bytes: number;
  width: number;
  height: number;
  expires_at: string;
}

export interface AddVisualAssetRequest {
  project_id: string;
  expected_revision: number;
  expected_version_id: string;
  operation_id: string;
  actor: string;
  origin: VideoVisualAssetOrigin;
  scene_id?: string;
  range: { start_us: number; end_us: number };
  fit: VideoVisualFit;
  crop?: VideoCanvasBounds;
  z_index: number;
  motion: VideoVisualMotion;
  transition_in_us: number;
  transition_out_us: number;
}

export interface AddVisualAssetResponse {
  project: VideoProject;
  asset_id: string;
  layer_id: string;
  job_id: string;
  replayed: boolean;
}

export interface CreateVideoProjectRequest {
  prompt: string;
  audio_file?: File;
  audio_local_path?: string;
  audio_display_name?: string;
  source_project_id?: string;
}

export type VideoScenePatch = Partial<Pick<VideoScene,
  | "layout"
  | "crop_mode"
  | "crop_rect"
  | "captions_enabled"
  | "caption_style"
  | "caption_bounds"
  | "voice_gain_db"
  | "music_gain_db"
  | "voice_id"
  | "model_id"
  | "speaker"
  | "language"
>>;

export interface ReviseVideoRequest {
  project_id: string;
  instruction: string;
  base_version_id: string;
  scene_id?: string;
  scene_patch?: VideoScenePatch;
}

export type VideoTimelineOperation =
  | { type: "split_scene"; scene_id: string; at_timeline_us: number }
  | { type: "trim_scene"; scene_id: string; source_start_us: number; source_end_us: number }
  | { type: "reorder_scene"; scene_id: string; to_index: number }
  | { type: "merge_scenes"; first_scene_id: string; second_scene_id: string }
  | {
    type: "update_visual_layer";
    layer_id: string;
    scene_id: string | null;
    range: { start_us: number; end_us: number };
    fit: VideoVisualFit;
    crop: VideoCanvasBounds | null;
    z_index: number;
    motion: VideoVisualMotion;
    transition_in_us: number;
    transition_out_us: number;
  };

export interface VideoTimelineEditRequest {
  project_id: string;
  expected_revision: number;
  base_version_id: string;
  operation_id: string;
  operations: VideoTimelineOperation[];
}

export type VideoRevisionStage =
  | "ingest"
  | "transcript"
  | "analysis"
  | "plan"
  | "speech"
  | "music"
  | "captions"
  | "tracking"
  | "scene_render"
  | "preview"
  | "final_render"
  | "publish_package";

export interface VideoTimelineChangeReceipt {
  project_id: string;
  expected_revision: number;
  base_version_id: string;
  operation_id: string;
  changed_paths: string[];
  invalidated_stages: VideoRevisionStage[];
}

export interface VideoTimelineEditResponse {
  project: VideoProject;
  receipt: VideoTimelineChangeReceipt;
  job_id: string;
  replayed: boolean;
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
  /** The renderer's caption design catalog, used for the preset chips and the live preview. */
  captionPresets(): Promise<VideoCaptionPreset[]>;
  /**
   * Copy an export to a location the user picks, returning the destination or `undefined` when the
   * save is cancelled. A plain `<a download>` cannot do this: the media origin is cross-origin to
   * the app, so the browser ignores `download` and navigates the window to the file instead.
   */
  saveArtifact(localPath: string, suggestedName?: string): Promise<string | undefined>;
  importLink(request: ImportLinkRequest): Promise<VideoProject>;
  pickLocalVideo?(): Promise<LocalVideoSelection | undefined>;
  pickLocalAudio?(): Promise<LocalAudioSelection | undefined>;
  chooseVideoVisualAsset(request: AuthorizeVisualSelectionRequest): Promise<VisualSourceReceipt | null>;
  importLocalVideo(request: ImportLocalVideoRequest): Promise<VideoProject>;
  analyzeVideo(projectId: string, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  planVideo(projectId: string, selectedCandidateIds?: string[]): Promise<VideoProject>;
  createVideoProject(request: CreateVideoProjectRequest): Promise<VideoProject>;
  listVideoProjects(): Promise<VideoProjectSummary[]>;
  getVideoProject(projectId: string): Promise<VideoProject>;
  renderVideoPreview(projectId: string, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  editVideoTimeline(request: VideoTimelineEditRequest): Promise<VideoTimelineEditResponse>;
  addVideoVisualAsset(request: AddVisualAssetRequest): Promise<AddVisualAssetResponse>;
  reviseVideo(request: ReviseVideoRequest): Promise<VideoProject>;
  exportVideo(request: VideoExportRequest, onProgress?: (update: VideoProgressUpdate) => void): Promise<VideoProject>;
  exportPublishPackage(projectId: string): Promise<VideoArtifact>;
  cancelVideoJob(jobId: string): Promise<boolean>;
  resumeVideoJob(jobId: string): Promise<VideoJob>;
  getToolStatus(): Promise<VideoToolStatus[]>;
}
