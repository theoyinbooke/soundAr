import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  CandidateVideoClip,
  CreateVideoProjectRequest,
  ImportLinkRequest,
  ImportLocalVideoRequest,
  LocalAudioSelection,
  LocalVideoSelection,
  ReviseVideoRequest,
  VideoArtifact,
  VideoExportRequest,
  VideoJob,
  VideoLinkPreview,
  VideoProgressUpdate,
  VideoProject,
  VideoProjectManifest,
  VideoProjectSummary,
  VideoScene,
  VideoStudioService,
  VideoTimelineManifest,
  VideoToolStatus,
} from "../types/video";

const FIXTURE_VIDEO_URL = "data:video/mp4;base64,AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAMObW9vdgAAAGxtdmhkAAAAAAAAAAAAAAAAAAAD6AAAA+gAAQAAAQAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAjl0cmFrAAAAXHRraGQAAAADAAAAAAAAAAAAAAABAAAAAAAAA+gAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAEAAAABwAAAAAAAkZWR0cwAAABxlbHN0AAAAAAAAAAEAAAPoAAAAAAABAAAAAAGxbWRpYQAAACBtZGhkAAAAAAAAAAAAAAAAAABAAAAAQABVxAAAAAAALWhkbHIAAAAAAAAAAHZpZGUAAAAAAAAAAAAAAABWaWRlb0hhbmRsZXIAAAABXG1pbmYAAAAUdm1oZAAAAAEAAAAAAAAAAAAAACRkaW5mAAAAHGRyZWYAAAAAAAAAAQAAAAx1cmwgAAAAAQAAARxzdGJsAAAAuHN0c2QAAAAAAAAAAQAAAKhhdmMxAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAEAAcABIAAAASAAAAAAAAAABFUxhdmM2Mi4xMS4xMDAgbGlieDI2NAAAAAAAAAAAAAAAGP//AAAALmF2Y0MBQsAK/+EAFmdCwAraEPsBEAAAAwAQAAADACDxImoBAAVozgGXIAAAABBwYXNwAAAAAQAAAAEAAAAUYnRydAAAAAAAABNQAAAAAAAAABhzdHRzAAAAAAAAAAEAAAABAABAAAAAABxzdHNjAAAAAAAAAAEAAAABAAAAAQAAAAEAAAAUc3RzegAAAAAAAAJqAAAAAQAAABRzdGNvAAAAAAAAAAEAAAM+AAAAYXVkdGEAAABZbWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAbWRpcmFwcGwAAAAAAAAAAAAAAAAsaWxzdAAAACSpdG9vAAAAHGRhdGEAAAABAAAAAExhdmY2Mi4zLjEwMAAAAAhmcmVlAAACcm1kYXQAAAJFBgX//0HcRem95tlIt5Ys2CDZI+7veDI2NCAtIGNvcmUgMTY1IC0gSC4yNjQvTVBFRy00IEFWQyBjb2RlYyAtIENvcHlsZWZ0IDIwMDMtMjAyNSAtIGh0dHA6Ly93d3cudmlkZW9sYW4ub3JnL3gyNjQuaHRtbCAtIG9wdGlvbnM6IGNhYmFjPTAgcmVmPTEgZGVibG9jaz0wOjA6MCBhbmFseXNlPTA6MCBtZT1kaWEgc3VibWU9MCBwc3k9MSBwc3lfcmQ9MS4wMDowLjAwIG1peGVkX3JlZj0wIG1lX3JhbmdlPTE2IGNocm9tYV9tZT0xIHRyZWxsaXM9MCA4eDhkY3Q9MCBjcW09MCBkZWFkem9uZT0yMSwxMSBmYXN0X3Bza2lwPTEgY2hyb21hX3FwX29mZnNldD0wIHRocmVhZHM9MyBsb29rYWhlYWRfdGhyZWFkcz0xIHNsaWNlZF90aHJlYWRzPTAgbnI9MCBkZWNpbWF0ZT0xIGludGVybGFjZWQ9MCBibHVyYXlfY29tcGF0PTAgY29uc3RyYWluZWRfaW50cmE9MCBiZnJhbWVzPTAgd2VpZ2h0cD0wIGtleWludD0yNTAga2V5aW50X21pbj0xIHNjZW5lY3V0PTAgaW50cmFfcmVmcmVzaD0wIHJjPWNyZiBtYnRyZWU9MCBjcmY9NTEuMCBxY29tcD0wLjYwIHFwbWluPTAgcXBtYXg9NjkgcXBzdGVwPTQgaXBfcmF0aW89MS40MCBhcT0wAIAAAAAdZYiEOiYoADJycnXXXXXXXXXXXXXXXXXXXXXXXXg=";
const FIXTURE_PUBLISH_PACKAGE_URL = "data:application/zip;base64,UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";
const FIXED_NOW = "2026-08-27T20:24:18.000Z";

const candidateSeed: CandidateVideoClip[] = [
  { id: "clip-1", rank: 1, source_start_ms: 14_000, source_end_ms: 32_000, title: "Hook: where I’ve been", transcript: "Where I’ve been and why the workflow needed to change.", score: 92, selected: true },
  { id: "clip-2", rank: 2, source_start_ms: 62_000, source_end_ms: 81_000, title: "Big update overview", transcript: "The big update makes local production faster and easier to review.", score: 91, selected: true },
  { id: "clip-3", rank: 3, source_start_ms: 168_000, source_end_ms: 188_000, title: "Tooling improvements", transcript: "A closer look at the tooling improvements.", score: 78, selected: false },
  { id: "clip-4", rank: 4, source_start_ms: 251_000, source_end_ms: 273_000, title: "What’s next", transcript: "What’s next for faster workflows and smarter audio.", score: 88, selected: true },
  { id: "clip-5", rank: 5, source_start_ms: 341_000, source_end_ms: 362_000, title: "Community shoutouts", transcript: "A quick thank-you to the people who tested the release.", score: 65, selected: false },
  { id: "clip-6", rank: 6, source_start_ms: 402_000, source_end_ms: 425_000, title: "Q&A preview", transcript: "A preview of the questions covered next.", score: 60, selected: false },
];

function clone<T>(value: T): T {
  return structuredClone(value);
}

function makeTimeline(scenes: VideoScene[], sourceDurationMs: number): VideoTimelineManifest {
  const videoItems = scenes.flatMap((scene, index) => {
    const clip = {
      id: `video-${scene.id}`,
      track: "video" as const,
      kind: "clip" as const,
      start_ms: scene.timeline_start_ms,
      end_ms: scene.timeline_end_ms,
      label: scene.title,
      scene_id: scene.id,
      source_start_ms: scene.source_start_ms,
      source_end_ms: scene.source_end_ms,
    };
    if (!index) return [clip];
    return [{
      id: `gap-${scene.id}`,
      track: "video" as const,
      kind: "gap" as const,
      start_ms: scenes[index - 1].timeline_end_ms,
      end_ms: scene.timeline_start_ms,
      label: "Preserved silent gap",
    }, clip];
  });
  const durationMs = scenes.at(-1)?.timeline_end_ms ?? 0;
  const aligned = (track: "captions" | "voice") => scenes.map((scene) => ({
    id: `${track}-${scene.id}`,
    track,
    kind: "clip" as const,
    start_ms: scene.timeline_start_ms,
    end_ms: scene.timeline_end_ms,
    label: track === "captions" ? scene.title : `${scene.title} voice`,
    scene_id: scene.id,
    source_start_ms: scene.source_start_ms,
    source_end_ms: scene.source_end_ms,
  }));
  return {
    duration_ms: durationMs,
    source_clock_duration_ms: sourceDurationMs,
    tracks: [
      { kind: "video", items: videoItems },
      { kind: "captions", items: aligned("captions") },
      { kind: "voice", items: aligned("voice") },
      { kind: "music", items: durationMs ? [{ id: "music-bed", track: "music", kind: "bed", start_ms: 0, end_ms: durationMs, label: "Calm ambient bed" }] : [] },
    ],
  };
}

function makeScenes(candidates: CandidateVideoClip[]): VideoScene[] {
  let cursor = 0;
  return candidates.filter((candidate) => candidate.selected).map((candidate, index) => {
    if (index) cursor += 5_500;
    const duration = candidate.source_end_ms - candidate.source_start_ms;
    const scene: VideoScene = {
      id: `scene-${candidate.id}`,
      candidate_id: candidate.id,
      position: index + 1,
      title: candidate.title,
      source_start_ms: candidate.source_start_ms,
      source_end_ms: candidate.source_end_ms,
      timeline_start_ms: cursor,
      timeline_end_ms: cursor + duration,
      transcript: candidate.transcript,
      layout: "portrait",
      crop_mode: "auto-center",
      captions_enabled: true,
      caption_style: "clean-white",
      voice_gain_db: 0,
      music_gain_db: -12,
    };
    cursor += duration;
    return scene;
  });
}

function makeMasterArtifact(projectId: string, versionId: string, durationMs: number, title = "Creator update · Portrait master"): VideoArtifact {
  return {
    id: `${projectId}-master`,
    project_id: projectId,
    version_id: versionId,
    role: "master",
    title,
    mime_type: "video/mp4",
    format: "mp4",
    url: FIXTURE_VIDEO_URL,
    download_name: `${projectId}-portrait-master.mp4`,
    duration_ms: durationMs,
    width: 1080,
    height: 1920,
    frame_rate: 30,
    codec: "H.264",
    file_size_bytes: 24_300_000,
    checksum: "a1b2c3d4…9f0e",
    playable: true,
    created_at: FIXED_NOW,
  };
}

function makeProject(id = "creator-update", name = "Creator update · Reel draft", status: VideoProject["status"] = "editing"): VideoProject {
  const candidates = clone(candidateSeed);
  const scenes = status === "review" || status === "analyzing" ? [] : makeScenes(candidates);
  const source = {
    id: `source-${id}`,
    kind: "link" as const,
    exact_url: "https://www.youtube.com/watch?v=creator-update",
    display_name: "creator-update.mp4",
    duration_ms: 565_000,
    width: 1920,
    height: 1080,
    mime_type: "video/mp4",
    rights_confirmed: true,
    rights_confirmed_at: FIXED_NOW,
    rights_confirmation_url: "https://www.youtube.com/watch?v=creator-update",
    preview_url: FIXTURE_VIDEO_URL,
    provenance: "Browser preview fixture · exact URL confirmation retained",
  };
  const manifest: VideoProjectManifest = {
    schema_version: 1,
    version_id: `${id}-v1`,
    source,
    transcript_version: "source-clock-v1",
    transcript: candidates.map((candidate) => ({ id: `transcript-${candidate.id}`, start_ms: candidate.source_start_ms, end_ms: candidate.source_end_ms, text: candidate.transcript, speaker: "Creator", source_clock: true })),
    candidates,
    scenes,
    timeline: makeTimeline(scenes, source.duration_ms),
    artifacts: [{ id: `${id}-proxy`, project_id: id, version_id: `${id}-v1`, role: "proxy", title: `${name} proxy`, mime_type: "video/mp4", format: "mp4", url: FIXTURE_VIDEO_URL, duration_ms: source.duration_ms, width: 360, height: 640, codec: "H.264", playable: true, created_at: FIXED_NOW }],
    revisions: [],
    settings: { aspect_ratio: "9:16", caption_style: "clean-white", captions_enabled: true, hardware_render: true },
  };
  const project: VideoProject = { id, name, status, duration_ms: manifest.timeline.duration_ms, scene_count: scenes.length, created_at: FIXED_NOW, updated_at: FIXED_NOW, poster_url: undefined, manifest };
  if (status === "exported") {
    const master = makeMasterArtifact(id, manifest.version_id, project.duration_ms);
    project.master = master;
    project.manifest.artifacts.push(master);
  }
  return project;
}

function summary(project: VideoProject): VideoProjectSummary {
  const { manifest: _manifest, created_at: _createdAt, ...projectSummary } = project;
  return clone(projectSummary);
}

function job(projectId: string, phase: VideoJob["phase"], progress: number, detail: string, status: VideoJob["status"] = "running"): VideoJob {
  return { id: `${projectId}-${phase}-job`, project_id: projectId, phase, progress, title: phase === "analyze" ? "Analyzing source" : phase === "preview" ? "Rendering preview" : "Exporting master", detail, status, durable: true, created_at: FIXED_NOW, updated_at: FIXED_NOW };
}

async function pause(milliseconds = 12): Promise<void> {
  await new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

export function createBrowserPreviewVideoService(): VideoStudioService {
  const projects = new Map<string, VideoProject>();
  [
    makeProject("creator-update-master", "Creator update · Reel master", "exported"),
    makeProject(),
    makeProject("product-demo", "Product demo", "review"),
    makeProject("tutorial-outline", "Tutorial outline"),
    makeProject("interview-cut", "Interview cut"),
    makeProject("brand-story", "Brand story"),
  ].forEach((project) => projects.set(project.id, project));
  let sequence = 1;
  const cancelledJobs = new Set<string>();

  const store = (project: VideoProject) => {
    projects.set(project.id, clone(project));
    return clone(project);
  };

  return {
    async previewLink(exactUrl) {
      let parsed: URL;
      try { parsed = new URL(exactUrl); } catch { throw new Error("Enter a valid HTTP or HTTPS video URL."); }
      if (!/^https?:$/.test(parsed.protocol)) throw new Error("Enter a valid HTTP or HTTPS video URL.");
      if (parsed.searchParams.has("list")) throw new Error("Playlists and collections are not supported. Use one exact video URL.");
      await pause();
      return { exact_url: exactUrl, title: "Big update: faster workflows and smarter audio", creator: "Creator Studio", duration_ms: 612_000, published_label: "May 20, 2025", view_label: "1.2M views", preview_url: FIXTURE_VIDEO_URL, is_single_source: true };
    },
    async importLink(request) {
      if (!request.rights_confirmed || request.rights_confirmation_url !== request.exact_url) throw new Error("Confirm rights for this exact URL before importing.");
      const project = makeProject(`link-project-${sequence++}`, "Imported source · Reel draft", "analyzing");
      project.manifest.source.exact_url = request.exact_url;
      project.manifest.source.rights_confirmation_url = request.rights_confirmation_url;
      project.manifest.source.rights_confirmed_at = FIXED_NOW;
      project.manifest.candidates = [];
      project.manifest.transcript = [];
      project.manifest.scenes = [];
      project.manifest.timeline = makeTimeline([], project.manifest.source.duration_ms);
      return store(project);
    },
    async importLocalVideo(request) {
      if (!request.rights_confirmed || (!request.file && !request.local_path)) throw new Error("Choose a local video you are authorized to use.");
      const project = makeProject(`upload-project-${sequence++}`, `${request.display_name.replace(/\.[^.]+$/, "")} · Reel draft`, "analyzing");
      project.manifest.source = { ...project.manifest.source, id: `source-${project.id}`, kind: "local-video", exact_url: undefined, local_path: request.local_path, display_name: request.display_name, rights_confirmation_url: undefined, provenance: "User-selected local media", rights_confirmed: true };
      project.manifest.candidates = [];
      project.manifest.transcript = [];
      project.manifest.scenes = [];
      project.manifest.timeline = makeTimeline([], project.manifest.source.duration_ms);
      return store(project);
    },
    async analyzeVideo(projectId, onProgress) {
      const current = projects.get(projectId);
      if (!current) throw new Error("Video project was not found.");
      for (const [progress, detail] of [[0.18, "Conforming a low-resolution proxy"], [0.46, "Transcribing on the source clock"], [0.72, "Scoring candidate moments"], [1, "Analysis ready for review"]] as const) {
        const update = job(projectId, "analyze", progress, detail, progress === 1 ? "completed" : "running");
        onProgress?.({ job: update, partial_artifact: progress === 0.18 ? current.manifest.artifacts.find((artifact) => artifact.role === "proxy") : undefined });
        await pause();
        if (cancelledJobs.delete(update.id)) throw new Error("Video analysis was cancelled.");
      }
      const analyzed = clone(current);
      analyzed.status = "review";
      analyzed.manifest.candidates = clone(candidateSeed);
      analyzed.manifest.transcript = candidateSeed.map((candidate) => ({ id: `transcript-${candidate.id}`, start_ms: candidate.source_start_ms, end_ms: candidate.source_end_ms, text: candidate.transcript, speaker: "Creator", source_clock: true }));
      analyzed.updated_at = FIXED_NOW;
      return store(analyzed);
    },
    async planVideo(projectId, selectedCandidateIds) {
      const current = projects.get(projectId);
      if (!current) throw new Error("Video project was not found.");
      const planned = clone(current);
      const selected = new Set(selectedCandidateIds ?? planned.manifest.candidates.filter((candidate) => candidate.selected).map((candidate) => candidate.id));
      planned.manifest.candidates = planned.manifest.candidates.map((candidate) => ({ ...candidate, selected: selected.has(candidate.id) }));
      planned.manifest.scenes = makeScenes(planned.manifest.candidates);
      planned.manifest.timeline = makeTimeline(planned.manifest.scenes, planned.manifest.source.duration_ms);
      planned.duration_ms = planned.manifest.timeline.duration_ms;
      planned.scene_count = planned.manifest.scenes.length;
      planned.status = "editing";
      await pause();
      return store(planned);
    },
    async createVideoProject(request) {
      if (!request.prompt.trim() && !request.audio_file && !request.audio_local_path) throw new Error("Add a prompt or an audio source.");
      const project = makeProject(`prompt-project-${sequence++}`, "Prompt concept · Video draft", "editing");
      const hasAudio = Boolean(request.audio_file || request.audio_local_path);
      project.manifest.source = { ...project.manifest.source, id: `source-${project.id}`, kind: hasAudio ? "audio" : "prompt", display_name: request.audio_display_name ?? request.audio_file?.name ?? "Prompt brief", local_path: request.audio_local_path, duration_ms: 70_000, exact_url: undefined, rights_confirmation_url: undefined, provenance: hasAudio ? "User-selected soundAr audio" : request.prompt.trim(), rights_confirmed: true };
      const generatedCandidates: CandidateVideoClip[] = [
        { id: "generated-1", rank: 1, source_start_ms: 0, source_end_ms: 18_000, title: "Opening promise", transcript: request.prompt.trim() || "A clear opening for this animated audio story.", score: 96, selected: true },
        { id: "generated-2", rank: 2, source_start_ms: 23_500, source_end_ms: 42_500, title: "Main story", transcript: "The central idea unfolds with calm captions and a focused portrait layout.", score: 94, selected: true },
        { id: "generated-3", rank: 3, source_start_ms: 48_000, source_end_ms: 70_000, title: "Closing card", transcript: "End with a concise takeaway and a clean final beat.", score: 91, selected: true },
      ];
      project.manifest.candidates = generatedCandidates;
      project.manifest.transcript = generatedCandidates.map((candidate) => ({ id: `transcript-${candidate.id}`, start_ms: candidate.source_start_ms, end_ms: candidate.source_end_ms, text: candidate.transcript, speaker: hasAudio ? "Speaker" : "Narrator", source_clock: true }));
      project.manifest.scenes = makeScenes(generatedCandidates);
      project.manifest.timeline = makeTimeline(project.manifest.scenes, project.manifest.source.duration_ms);
      project.manifest.artifacts = project.manifest.artifacts.map((artifact) => ({ ...artifact, id: `${project.id}-proxy`, project_id: project.id, version_id: project.manifest.version_id, title: `${project.name} animated proxy`, duration_ms: project.manifest.timeline.duration_ms }));
      project.duration_ms = project.manifest.timeline.duration_ms;
      project.scene_count = project.manifest.scenes.length;
      return store(project);
    },
    async listVideoProjects() { return [...projects.values()].map(summary); },
    async getVideoProject(projectId) {
      const project = projects.get(projectId);
      if (!project) throw new Error("Video project was not found.");
      return clone(project);
    },
    async renderVideoPreview(projectId, onProgress) {
      const current = projects.get(projectId);
      if (!current) throw new Error("Video project was not found.");
      for (const [progress, detail] of [[0.24, "Reusing unchanged scene renders"], [0.63, "Compositing captions and audio"], [1, "Preview is playable"]] as const) {
        const update = job(projectId, "preview", progress, detail, progress === 1 ? "completed" : "running");
        onProgress?.({ job: update });
        await pause();
        if (cancelledJobs.delete(update.id)) throw new Error("Preview render was cancelled.");
      }
      const rendered = clone(current);
      rendered.status = "editing";
      rendered.manifest.artifacts.push({ id: `${projectId}-preview`, project_id: projectId, version_id: rendered.manifest.version_id, role: "preview", title: `${rendered.name} preview`, mime_type: "video/mp4", format: "mp4", url: FIXTURE_VIDEO_URL, download_name: `${projectId}-preview.mp4`, duration_ms: rendered.duration_ms, width: 540, height: 960, frame_rate: 30, codec: "H.264", file_size_bytes: 1_448, playable: true, created_at: FIXED_NOW });
      return store(rendered);
    },
    async reviseVideo(request) {
      const current = projects.get(request.project_id);
      if (!current) throw new Error("Video project was not found.");
      if (current.manifest.version_id !== request.base_version_id) throw new Error("The project changed. Refresh before applying this revision.");
      const revised = clone(current);
      const priorVersion = revised.manifest.version_id;
      revised.manifest.version_id = `${request.project_id}-v${revised.manifest.revisions.length + 2}`;
      const requestedCaptionStyle = request.instruction.toLowerCase().includes("calm") ? "calm" : revised.manifest.settings.caption_style;
      revised.manifest.settings.caption_style = requestedCaptionStyle;
      revised.manifest.scenes = revised.manifest.scenes.map((scene) => scene.id === request.scene_id && request.scene_patch
        ? { ...scene, ...request.scene_patch }
        : request.scene_id ? scene : { ...scene, caption_style: requestedCaptionStyle });
      revised.manifest.artifacts = revised.manifest.artifacts.filter((artifact) => !["preview", "master", "variation"].includes(artifact.role));
      revised.manifest.revisions.push({ id: `revision-${revised.manifest.revisions.length + 1}`, created_at: FIXED_NOW, instruction: request.instruction, affected_stages: ["preview", "export"], base_version_id: priorVersion, version_id: revised.manifest.version_id });
      revised.status = "editing";
      revised.master = undefined;
      return store(revised);
    },
    async exportVideo(request, onProgress) {
      const current = projects.get(request.project_id);
      if (!current || current.manifest.version_id !== request.version_id) throw new Error("The requested video version is unavailable.");
      for (const [progress, detail] of [[0.28, "Encoding with the final hardware profile"], [0.76, "Publishing the master atomically"], [1, "Final MP4 is ready"]] as const) {
        const update = job(request.project_id, "export", progress, detail, progress === 1 ? "completed" : "running");
        onProgress?.({ job: update });
        await pause();
        if (cancelledJobs.delete(update.id)) throw new Error("Video export was cancelled.");
      }
      const exported = clone(current);
      const master = makeMasterArtifact(request.project_id, request.version_id, exported.duration_ms);
      exported.master = master;
      exported.status = "exported";
      exported.manifest.artifacts = [...exported.manifest.artifacts.filter((artifact) => artifact.role !== "master"), master];
      return store(exported);
    },
    async exportPublishPackage(projectId) {
      const current = projects.get(projectId);
      if (!current?.master) throw new Error("Export a final master before creating a publish package.");
      const artifact: VideoArtifact = { id: `${projectId}-publish-package`, project_id: projectId, version_id: current.manifest.version_id, role: "publish-package", title: `${current.name} publish package`, mime_type: "application/zip", format: "zip", url: FIXTURE_PUBLISH_PACKAGE_URL, download_name: `${projectId}-publish-package.zip`, playable: false, created_at: FIXED_NOW };
      current.manifest.artifacts.push(artifact);
      store(current);
      return clone(artifact);
    },
    async cancelVideoJob(jobId) { cancelledJobs.add(jobId); return true; },
    async resumeVideoJob(jobId) { cancelledJobs.delete(jobId); return { ...job(jobId.split("-")[0] || "video-project", "analyze", 0, "Resuming durable video job", "queued"), id: jobId }; },
    async getToolStatus() {
      return [
        { id: "ffmpeg", label: "FFmpeg", state: "ready", detail: "Hardware profiles detected" },
        { id: "ffprobe", label: "FFprobe", state: "ready" },
        { id: "yt-dlp", label: "yt-dlp", state: "ready", detail: "Single-source mode" },
        { id: "javascript", label: "JavaScript runtime", state: "ready", detail: "Node 22" },
        { id: "transcriber", label: "faster-whisper", state: "ready", detail: "CUDA" },
      ];
    },
  };
}

async function nativeWithProgress<T>(projectId: string, command: string, payload: Record<string, unknown>, onProgress?: (update: VideoProgressUpdate) => void): Promise<T> {
  const unlisten = onProgress ? await listen<VideoProgressUpdate>("video-job-progress", (event) => {
    if (event.payload.job.project_id === projectId) onProgress(event.payload);
  }) : undefined;
  try {
    return await invoke<T>(command, payload);
  } finally {
    unlisten?.();
  }
}

function withNativeArtifactUrl(artifact: VideoArtifact): VideoArtifact {
  return artifact.url || !artifact.local_path ? artifact : { ...artifact, url: convertFileSrc(artifact.local_path) };
}

function withNativeProjectUrls(project: VideoProject): VideoProject {
  const artifacts = project.manifest.artifacts.map(withNativeArtifactUrl);
  const master = project.master ? withNativeArtifactUrl(project.master) : undefined;
  return {
    ...project,
    master,
    manifest: {
      ...project.manifest,
      source: project.manifest.source.preview_url || !project.manifest.source.local_path
        ? project.manifest.source
        : { ...project.manifest.source, preview_url: convertFileSrc(project.manifest.source.local_path) },
      artifacts,
    },
  };
}

function withNativeSummaryUrls(project: VideoProjectSummary): VideoProjectSummary {
  return { ...project, master: project.master ? withNativeArtifactUrl(project.master) : undefined };
}

function createNativeVideoService(): VideoStudioService {
  return {
    previewLink: (exactUrl) => invoke<VideoLinkPreview>("preview_video_link", { exactUrl }),
    importLink: (request) => invoke<VideoProject>("import_video_link", { request }).then(withNativeProjectUrls),
    async pickLocalVideo(): Promise<LocalVideoSelection | undefined> {
      const selected = await open({ multiple: false, directory: false, filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "webm", "m4v"] }] });
      return typeof selected === "string" ? { local_path: selected, display_name: selected.split(/[\\/]/).at(-1) ?? "video" } : undefined;
    },
    async pickLocalAudio(): Promise<LocalAudioSelection | undefined> {
      const selected = await open({ multiple: false, directory: false, filters: [{ name: "Audio", extensions: ["wav", "flac", "mp3", "m4a", "ogg"] }] });
      return typeof selected === "string" ? { local_path: selected, display_name: selected.split(/[\\/]/).at(-1) ?? "audio" } : undefined;
    },
    importLocalVideo: (request: ImportLocalVideoRequest) => {
      if (!request.local_path) throw new Error("Choose a local video through the desktop file picker.");
      return invoke<VideoProject>("import_video_file", { request: { ...request, file: undefined } }).then(withNativeProjectUrls);
    },
    analyzeVideo: (projectId, onProgress) => nativeWithProgress<VideoProject>(projectId, "analyze_video", { projectId }, onProgress).then(withNativeProjectUrls),
    planVideo: (projectId, selectedCandidateIds) => invoke<VideoProject>("plan_video", { projectId, selectedCandidateIds }).then(withNativeProjectUrls),
    createVideoProject: (request: CreateVideoProjectRequest) => invoke<VideoProject>("create_video_project", { request: { ...request, audio_file: undefined } }).then(withNativeProjectUrls),
    listVideoProjects: () => invoke<VideoProjectSummary[]>("list_video_projects").then((projects) => projects.map(withNativeSummaryUrls)),
    getVideoProject: (projectId) => invoke<VideoProject>("get_video_project", { projectId }).then(withNativeProjectUrls),
    renderVideoPreview: (projectId, onProgress) => nativeWithProgress<VideoProject>(projectId, "render_video_preview", { projectId }, onProgress).then(withNativeProjectUrls),
    reviseVideo: (request: ReviseVideoRequest) => invoke<VideoProject>("revise_video", { request }).then(withNativeProjectUrls),
    exportVideo: (request: VideoExportRequest, onProgress) => nativeWithProgress<VideoProject>(request.project_id, "export_video", { request }, onProgress).then(withNativeProjectUrls),
    exportPublishPackage: (projectId) => invoke<VideoArtifact>("export_publish_package", { projectId }).then(withNativeArtifactUrl),
    cancelVideoJob: (jobId) => invoke<boolean>("cancel_video_job", { jobId }),
    resumeVideoJob: (jobId) => invoke<VideoJob>("resume_video_job", { jobId }),
    getToolStatus: () => invoke<VideoToolStatus[]>("video_runtime_status"),
  };
}

export function createVideoStudioService(): VideoStudioService {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window
    ? createNativeVideoService()
    : createBrowserPreviewVideoService();
}
