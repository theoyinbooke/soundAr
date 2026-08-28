import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AddVisualAssetRequest,
  AddVisualAssetResponse,
  AuthorizeVisualSelectionRequest,
  CandidateVideoClip,
  CreateVideoProjectRequest,
  ImportLinkRequest,
  ImportLocalVideoRequest,
  LocalAudioSelection,
  LocalVideoSelection,
  ReviseVideoRequest,
  VideoArtifact,
  VideoCanvasBounds,
  VideoCaptionPage,
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
  VideoTimelineEditRequest,
  VideoTimelineEditResponse,
  VideoToolStatus,
  VideoVisualAsset,
  VideoVisualLayer,
  VisualSourceReceipt,
} from "../types/video";

const FIXTURE_VIDEO_URL = "data:video/mp4;base64,AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAMObW9vdgAAAGxtdmhkAAAAAAAAAAAAAAAAAAAD6AAAA+gAAQAAAQAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAjl0cmFrAAAAXHRraGQAAAADAAAAAAAAAAAAAAABAAAAAAAAA+gAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAEAAAABwAAAAAAAkZWR0cwAAABxlbHN0AAAAAAAAAAEAAAPoAAAAAAABAAAAAAGxbWRpYQAAACBtZGhkAAAAAAAAAAAAAAAAAABAAAAAQABVxAAAAAAALWhkbHIAAAAAAAAAAHZpZGUAAAAAAAAAAAAAAABWaWRlb0hhbmRsZXIAAAABXG1pbmYAAAAUdm1oZAAAAAEAAAAAAAAAAAAAACRkaW5mAAAAHGRyZWYAAAAAAAAAAQAAAAx1cmwgAAAAAQAAARxzdGJsAAAAuHN0c2QAAAAAAAAAAQAAAKhhdmMxAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAEAAcABIAAAASAAAAAAAAAABFUxhdmM2Mi4xMS4xMDAgbGlieDI2NAAAAAAAAAAAAAAAGP//AAAALmF2Y0MBQsAK/+EAFmdCwAraEPsBEAAAAwAQAAADACDxImoBAAVozgGXIAAAABBwYXNwAAAAAQAAAAEAAAAUYnRydAAAAAAAABNQAAAAAAAAABhzdHRzAAAAAAAAAAEAAAABAABAAAAAABxzdHNjAAAAAAAAAAEAAAABAAAAAQAAAAEAAAAUc3RzegAAAAAAAAJqAAAAAQAAABRzdGNvAAAAAAAAAAEAAAM+AAAAYXVkdGEAAABZbWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAbWRpcmFwcGwAAAAAAAAAAAAAAAAsaWxzdAAAACSpdG9vAAAAHGRhdGEAAAABAAAAAExhdmY2Mi4zLjEwMAAAAAhmcmVlAAACcm1kYXQAAAJFBgX//0HcRem95tlIt5Ys2CDZI+7veDI2NCAtIGNvcmUgMTY1IC0gSC4yNjQvTVBFRy00IEFWQyBjb2RlYyAtIENvcHlsZWZ0IDIwMDMtMjAyNSAtIGh0dHA6Ly93d3cudmlkZW9sYW4ub3JnL3gyNjQuaHRtbCAtIG9wdGlvbnM6IGNhYmFjPTAgcmVmPTEgZGVibG9jaz0wOjA6MCBhbmFseXNlPTA6MCBtZT1kaWEgc3VibWU9MCBwc3k9MSBwc3lfcmQ9MS4wMDowLjAwIG1peGVkX3JlZj0wIG1lX3JhbmdlPTE2IGNocm9tYV9tZT0xIHRyZWxsaXM9MCA4eDhkY3Q9MCBjcW09MCBkZWFkem9uZT0yMSwxMSBmYXN0X3Bza2lwPTEgY2hyb21hX3FwX29mZnNldD0wIHRocmVhZHM9MyBsb29rYWhlYWRfdGhyZWFkcz0xIHNsaWNlZF90aHJlYWRzPTAgbnI9MCBkZWNpbWF0ZT0xIGludGVybGFjZWQ9MCBibHVyYXlfY29tcGF0PTAgY29uc3RyYWluZWRfaW50cmE9MCBiZnJhbWVzPTAgd2VpZ2h0cD0wIGtleWludD0yNTAga2V5aW50X21pbj0xIHNjZW5lY3V0PTAgaW50cmFfcmVmcmVzaD0wIHJjPWNyZiBtYnRyZWU9MCBjcmY9NTEuMCBxY29tcD0wLjYwIHFwbWluPTAgcXBtYXg9NjkgcXBzdGVwPTQgaXBfcmF0aW89MS40MCBhcT0wAIAAAAAdZYiEOiYoADJycnXXXXXXXXXXXXXXXXXXXXXXXXg=";
const FIXTURE_PUBLISH_PACKAGE_URL = "data:application/zip;base64,UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";
const FIXTURE_VISUAL_URL = "/video-studio-editorial-visual.webp";
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

function makeCaptionPages(scenes: VideoScene[]): VideoCaptionPage[] {
  return scenes.flatMap((scene) => {
    const words = scene.transcript.trim().split(/\s+/).filter(Boolean);
    const chunks = Array.from({ length: Math.max(1, Math.ceil(words.length / 6)) }, (_, index) => words.slice(index * 6, (index + 1) * 6));
    const sceneDuration = Math.max(1, scene.timeline_end_ms - scene.timeline_start_ms);
    return chunks.map((chunk, pageIndex) => {
      const startMs = scene.timeline_start_ms + Math.round(sceneDuration * pageIndex / chunks.length);
      const endMs = scene.timeline_start_ms + Math.round(sceneDuration * (pageIndex + 1) / chunks.length);
      const pageDuration = Math.max(1, endMs - startMs);
      return {
        id: `caption-page-${scene.id}-${pageIndex + 1}`,
        cue_id: `transcript-${scene.candidate_id ?? scene.id}`,
        scene_id: scene.id,
        start_ms: startMs,
        end_ms: endMs,
        text: chunk.join(" "),
        style_id: scene.caption_style,
        bounds: scene.caption_bounds ?? { x_bp: 800, y_bp: 7350, width_bp: 8400, height_bp: 1500 },
        font_size_bp: 480,
        words: chunk.map((text, wordIndex) => ({
          text,
          start_ms: startMs + Math.round(pageDuration * wordIndex / Math.max(1, chunk.length)),
          end_ms: startMs + Math.round(pageDuration * (wordIndex + 1) / Math.max(1, chunk.length)),
        })),
      };
    });
  });
}

function makeTimeline(
  scenes: VideoScene[],
  sourceDurationMs: number,
  captionPages = makeCaptionPages(scenes),
  visualLayers: VideoVisualLayer[] = [],
  visualAssets: VideoVisualAsset[] = [],
): VideoTimelineManifest {
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
  const voiceItems = scenes.map((scene) => ({
    id: `voice-${scene.id}`,
    track: "voice" as const,
    kind: "clip" as const,
    start_ms: scene.timeline_start_ms,
    end_ms: scene.timeline_end_ms,
    label: `${scene.title} voice`,
    scene_id: scene.id,
    source_start_ms: scene.source_start_ms,
    source_end_ms: scene.source_end_ms,
  }));
  const visualTitleByAsset = new Map(visualAssets.map((asset) => [asset.id, asset.provenance.producer || "Visual"]));
  return {
    duration_ms: durationMs,
    source_clock_duration_ms: sourceDurationMs,
    tracks: [
      { kind: "video", items: videoItems },
      ...(visualLayers.length ? [{
        kind: "visuals" as const,
        items: visualLayers.map((layer) => ({
          id: layer.id,
          track: "visuals" as const,
          kind: "clip" as const,
          start_ms: layer.start_ms,
          end_ms: layer.end_ms,
          label: visualTitleByAsset.get(layer.asset_id) ?? "Visual",
          scene_id: layer.scene_id,
          asset_id: layer.asset_id,
          start_bounds: layer.motion.start_bounds,
          end_bounds: layer.motion.end_bounds,
          fit: layer.fit,
          z_index: layer.z_index,
        })),
      }] : []),
      { kind: "captions", items: captionPages.map((page) => ({ id: page.id, track: "captions", kind: "clip", start_ms: page.start_ms, end_ms: page.end_ms, label: page.text, scene_id: page.scene_id, caption_style: page.style_id, bounds: page.bounds, font_size_bp: page.font_size_bp })) },
      { kind: "voice", items: voiceItems },
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
      caption_bounds: { x_bp: 800, y_bp: 7350, width_bp: 8400, height_bp: 1500 },
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
    caption_pages: makeCaptionPages(scenes),
    candidates,
    scenes,
    narration_bindings: [],
    visual_assets: [],
    visual_layers: [],
    timeline: makeTimeline(scenes, source.duration_ms, makeCaptionPages(scenes)),
    artifacts: [{ id: `${id}-proxy`, project_id: id, version_id: `${id}-v1`, role: "proxy", title: `${name} proxy`, mime_type: "video/mp4", format: "mp4", url: FIXTURE_VIDEO_URL, duration_ms: source.duration_ms, width: 360, height: 640, codec: "H.264", playable: true, created_at: FIXED_NOW }],
    revisions: [],
    settings: { aspect_ratio: "9:16", caption_style: "clean-white", captions_enabled: true, hardware_render: true },
  };
  const project: VideoProject = { id, name, status, revision: 0, duration_ms: manifest.timeline.duration_ms, scene_count: scenes.length, created_at: FIXED_NOW, updated_at: FIXED_NOW, poster_url: undefined, manifest };
  if (status === "exported") {
    const master = makeMasterArtifact(id, manifest.version_id, project.duration_ms);
    project.master = master;
    project.deliverables = [master];
    project.manifest.artifacts.push(master);
  }
  return project;
}

function makeSourceRecoveryProject(): VideoProject {
  const project = makeProject(
    "source-recovery",
    "From interview source to a focused portrait story with a concise opening",
    "failed",
  );
  project.duration_ms = 0;
  project.scene_count = 0;
  project.manifest.source = {
    ...project.manifest.source,
    kind: "local-video",
    exact_url: undefined,
    rights_confirmation_url: undefined,
    display_name: "customer-interview-final-approved-long-source-filename.mp4",
    provenance: "User-selected local media",
  };
  project.manifest.transcript = [];
  project.manifest.caption_pages = [];
  project.manifest.candidates = [];
  project.manifest.scenes = [];
  project.manifest.timeline = makeTimeline([], project.manifest.source.duration_ms, []);
  project.recoverable_job = {
    id: "source-recovery-preview-job",
    project_id: project.id,
    phase: "preview",
    progress: 0.2,
    title: "Render preview",
    detail: "video.reviewed_scenes_required: Review at least one scene before rendering the timeline",
    status: "failed",
    error: "video.reviewed_scenes_required: Review at least one scene before rendering the timeline",
    durable: true,
    created_at: FIXED_NOW,
    updated_at: FIXED_NOW,
  };
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

function microsecondsToMilliseconds(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`video.invalid_timestamp: ${field} must be a non-negative safe integer.`);
  return value / 1_000;
}

function validateVisualRequest(request: AddVisualAssetRequest, project: VideoProject): void {
  if (!request.origin.receipt_id.trim() || !request.actor.trim() || !request.operation_id.trim()) {
    throw new Error("video.invalid_visual: Image receipt, actor, and operation identifier are required.");
  }
  if (!Number.isSafeInteger(request.range.start_us) || !Number.isSafeInteger(request.range.end_us)
    || request.range.start_us < 0 || request.range.end_us <= request.range.start_us) {
    throw new Error("video.invalid_timestamp: Image range must use exact, increasing microseconds.");
  }
  const projectEndUs = Math.round(project.duration_ms * 1_000);
  if (!Number.isSafeInteger(projectEndUs) || request.range.end_us > projectEndUs) {
    throw new Error("video.invalid_timestamp: Image range exceeds the project timeline.");
  }
  if (request.scene_id) {
    const scene = project.manifest.scenes.find((candidate) => candidate.id === request.scene_id);
    if (!scene || request.range.start_us < Math.round(scene.timeline_start_ms * 1_000)
      || request.range.end_us > Math.round(scene.timeline_end_ms * 1_000)) {
      throw new Error("video.invalid_timestamp: Scene image range must remain inside the selected scene.");
    }
  }
  validateVisualLayerFields(request);
}

function validateVisualLayerFields(request: {
  range: { start_us: number; end_us: number };
  motion: AddVisualAssetRequest["motion"];
  crop?: VideoCanvasBounds | null;
  z_index: number;
  transition_in_us: number;
  transition_out_us: number;
}): void {
  const bounds = [request.motion.start_bounds, request.motion.end_bounds, ...(request.crop ? [request.crop] : [])];
  for (const rect of bounds) {
    const values = [rect.x_bp, rect.y_bp, rect.width_bp, rect.height_bp];
    if (!values.every(Number.isSafeInteger) || rect.x_bp < 0 || rect.y_bp < 0 || rect.width_bp <= 0 || rect.height_bp <= 0
      || rect.x_bp + rect.width_bp > 10_000 || rect.y_bp + rect.height_bp > 10_000) {
      throw new Error("video.invalid_layout: Image bounds must fit the normalized canvas.");
    }
  }
  if (request.motion.start_bounds.width_bp * request.motion.end_bounds.height_bp
    !== request.motion.end_bounds.width_bp * request.motion.start_bounds.height_bp
    || request.motion.start_opacity_milli !== request.motion.end_opacity_milli
    || request.motion.start_opacity_milli < 0 || request.motion.start_opacity_milli > 1_000
    || request.motion.start_rotation_milli_degrees !== 0 || request.motion.end_rotation_milli_degrees !== 0) {
    throw new Error("video.invalid_layout: Image motion is outside the supported renderer envelope.");
  }
  if (!Number.isSafeInteger(request.z_index) || request.z_index < -32_768 || request.z_index > 32_767
    || !Number.isSafeInteger(request.transition_in_us) || !Number.isSafeInteger(request.transition_out_us)
    || request.transition_in_us < 0 || request.transition_out_us < 0
    || request.transition_in_us + request.transition_out_us > request.range.end_us - request.range.start_us) {
    throw new Error("video.invalid_layout: Image layer ordering or fades are invalid.");
  }
}

function applyBrowserTimelineOperations(project: VideoProject, request: VideoTimelineEditRequest): VideoProject {
  if (!request.operations.length || request.operations.length > 100) throw new Error("video.invalid_scene: Add between one and 100 timeline operations.");
  const edited = clone(project);
  const originalVersion = edited.manifest.version_id;
  const originalScenes = new Map(edited.manifest.scenes.map((scene) => [scene.id, scene]));
  request.operations.forEach((operation) => {
    const scenes = edited.manifest.scenes;
    if (operation.type === "split_scene") {
      const index = scenes.findIndex((scene) => scene.id === operation.scene_id);
      if (index < 0) throw new Error("video.missing_reference: The scene to split no longer exists.");
      const scene = scenes[index];
      const splitMs = microsecondsToMilliseconds(operation.at_timeline_us, "at_timeline_us");
      if (splitMs - scene.timeline_start_ms < 100 || scene.timeline_end_ms - splitMs < 100) throw new Error("video.invalid_scene: Both split scenes must be at least 100 milliseconds.");
      const timelineDuration = scene.timeline_end_ms - scene.timeline_start_ms;
      const sourceDuration = scene.source_end_ms - scene.source_start_ms;
      const sourceSplit = scene.source_start_ms + (splitMs - scene.timeline_start_ms) * sourceDuration / timelineDuration;
      const words = scene.transcript.trim().split(/\s+/).filter(Boolean);
      const wordIndex = Math.max(1, Math.min(words.length - 1, Math.round(words.length * (splitMs - scene.timeline_start_ms) / timelineDuration)));
      const left = { ...scene, source_end_ms: sourceSplit, timeline_end_ms: splitMs, transcript: words.slice(0, wordIndex).join(" ") };
      const right = { ...scene, id: `scene-split-${scene.id}-${Math.round(splitMs * 1_000)}`, source_start_ms: sourceSplit, timeline_start_ms: splitMs, transcript: words.slice(wordIndex).join(" ") };
      scenes.splice(index, 1, left, right);
    } else if (operation.type === "trim_scene") {
      const index = scenes.findIndex((scene) => scene.id === operation.scene_id);
      if (index < 0) throw new Error("video.missing_reference: The scene to trim no longer exists.");
      const scene = scenes[index];
      const sourceStart = microsecondsToMilliseconds(operation.source_start_us, "source_start_us");
      const sourceEnd = microsecondsToMilliseconds(operation.source_end_us, "source_end_us");
      if (sourceStart < scene.source_start_ms || sourceEnd > scene.source_end_ms || sourceEnd - sourceStart < 100) throw new Error("video.invalid_timestamp: Trim bounds must retain at least 100 milliseconds inside the scene.");
      const timelineDuration = scene.timeline_end_ms - scene.timeline_start_ms;
      const sourceDuration = scene.source_end_ms - scene.source_start_ms;
      const nextDuration = timelineDuration * (sourceEnd - sourceStart) / sourceDuration;
      const removed = timelineDuration - nextDuration;
      scenes[index] = { ...scene, source_start_ms: sourceStart, source_end_ms: sourceEnd, timeline_end_ms: scene.timeline_start_ms + nextDuration };
      for (let later = index + 1; later < scenes.length; later += 1) {
        scenes[later] = { ...scenes[later], timeline_start_ms: scenes[later].timeline_start_ms - removed, timeline_end_ms: scenes[later].timeline_end_ms - removed };
      }
    } else if (operation.type === "reorder_scene") {
      const fromIndex = scenes.findIndex((scene) => scene.id === operation.scene_id);
      if (fromIndex < 0 || operation.to_index < 0 || operation.to_index >= scenes.length || !Number.isSafeInteger(operation.to_index)) throw new Error("video.invalid_scene: Reorder target is outside the scene list.");
      if (fromIndex === operation.to_index) throw new Error("video.invalid_scene: Reorder must change the scene position.");
      const gaps = scenes.slice(1).map((scene, index) => Math.max(0, scene.timeline_start_ms - scenes[index].timeline_end_ms));
      const prefix = scenes[0]?.timeline_start_ms ?? 0;
      const [moved] = scenes.splice(fromIndex, 1);
      scenes.splice(operation.to_index, 0, moved);
      let cursor = prefix;
      scenes.forEach((scene, index) => {
        const duration = scene.timeline_end_ms - scene.timeline_start_ms;
        scenes[index] = { ...scene, timeline_start_ms: cursor, timeline_end_ms: cursor + duration };
        cursor += duration + (gaps[index] ?? 0);
      });
    } else if (operation.type === "merge_scenes") {
      const firstIndex = scenes.findIndex((scene) => scene.id === operation.first_scene_id);
      const secondIndex = scenes.findIndex((scene) => scene.id === operation.second_scene_id);
      if (firstIndex < 0 || secondIndex !== firstIndex + 1) throw new Error("video.invalid_scene: Merge requires adjacent split scenes.");
      const first = scenes[firstIndex];
      const second = scenes[secondIndex];
      if (Math.abs(first.timeline_end_ms - second.timeline_start_ms) > 0.001) throw new Error("video.invalid_scene: Merge requires contiguous split scenes.");
      scenes.splice(firstIndex, 2, { ...first, source_end_ms: second.source_end_ms, timeline_end_ms: second.timeline_end_ms, transcript: `${first.transcript} ${second.transcript}`.trim() });
    } else {
      const layerIndex = (edited.manifest.visual_layers ?? []).findIndex((layer) => layer.id === operation.layer_id);
      if (layerIndex < 0) throw new Error("video.missing_reference: The image layer no longer exists.");
      const startMs = microsecondsToMilliseconds(operation.range.start_us, "visual range start_us");
      const endMs = microsecondsToMilliseconds(operation.range.end_us, "visual range end_us");
      const scene = operation.scene_id ? scenes.find((candidate) => candidate.id === operation.scene_id) : undefined;
      if (endMs <= startMs || endMs > edited.duration_ms || (operation.scene_id && (!scene || startMs < scene.timeline_start_ms || endMs > scene.timeline_end_ms))) {
        throw new Error("video.invalid_timestamp: Image range must remain inside its selected scene and project.");
      }
      validateVisualLayerFields(operation);
      edited.manifest.visual_layers![layerIndex] = {
        id: operation.layer_id,
        asset_id: edited.manifest.visual_layers![layerIndex].asset_id,
        scene_id: operation.scene_id,
        start_ms: startMs,
        end_ms: endMs,
        fit: operation.fit,
        crop: operation.crop,
        z_index: operation.z_index,
        motion: clone(operation.motion),
        transition_in_ms: operation.transition_in_us / 1_000,
        transition_out_ms: operation.transition_out_us / 1_000,
      };
    }
    edited.manifest.scenes = scenes.map((scene, index) => ({ ...scene, position: index + 1 }));
  });
  edited.revision += 1;
  edited.manifest.version_id = `${edited.id}-v${edited.revision + 1}`;
  edited.manifest.visual_layers = (edited.manifest.visual_layers ?? []).flatMap((layer) => {
    if (!layer.scene_id) return [layer];
    const before = originalScenes.get(layer.scene_id);
    const after = edited.manifest.scenes.find((scene) => scene.id === layer.scene_id);
    if (!before || !after) return [];
    const shift = after.timeline_start_ms - before.timeline_start_ms;
    const start = Math.max(after.timeline_start_ms, Math.min(after.timeline_end_ms - 1, layer.start_ms + shift));
    const end = Math.max(start + 1, Math.min(after.timeline_end_ms, layer.end_ms + shift));
    return [{ ...layer, start_ms: start, end_ms: end }];
  });
  edited.manifest.caption_pages = makeCaptionPages(edited.manifest.scenes);
  edited.manifest.timeline = makeTimeline(
    edited.manifest.scenes,
    edited.manifest.source.duration_ms,
    edited.manifest.caption_pages,
    edited.manifest.visual_layers,
    edited.manifest.visual_assets,
  );
  edited.duration_ms = edited.manifest.timeline.duration_ms;
  edited.scene_count = edited.manifest.scenes.length;
  edited.status = "editing";
  edited.master = undefined;
  edited.deliverables = [];
  edited.manifest.artifacts = edited.manifest.artifacts.filter((artifact) => !["preview", "master", "variation", "publish-package"].includes(artifact.role));
  edited.manifest.revisions.push({ id: `revision-${edited.manifest.revisions.length + 1}`, created_at: FIXED_NOW, instruction: "Edit the canonical timeline.", affected_stages: ["preview", "export"], base_version_id: originalVersion, version_id: edited.manifest.version_id });
  edited.updated_at = FIXED_NOW;
  return edited;
}

export function createBrowserPreviewVideoService(): VideoStudioService {
  const projects = new Map<string, VideoProject>();
  [
    makeProject("creator-update-master", "Creator update · Reel master", "exported"),
    makeSourceRecoveryProject(),
    makeProject(),
    makeProject("product-demo", "Product demo", "review"),
    makeProject("tutorial-outline", "Tutorial outline"),
    makeProject("interview-cut", "Interview cut"),
    makeProject("brand-story", "Brand story"),
  ].forEach((project) => projects.set(project.id, project));
  let sequence = 1;
  const cancelledJobs = new Set<string>();
  const timelineEditReplays = new Map<string, VideoTimelineEditResponse>();
  const visualAssetReplays = new Map<string, AddVisualAssetResponse>();
  const visualReceipts = new Map<string, VisualSourceReceipt & { consumed: boolean; localPath: string }>();
  let visualReceiptSequence = 1;

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
      project.manifest.caption_pages = [];
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
      project.manifest.caption_pages = [];
      project.manifest.scenes = [];
      project.manifest.timeline = makeTimeline([], project.manifest.source.duration_ms);
      return store(project);
    },
    async chooseVideoVisualAsset(request: AuthorizeVisualSelectionRequest): Promise<VisualSourceReceipt> {
      const current = projects.get(request.project_id);
      if (!current) throw new Error("Video project was not found.");
      if (current.revision !== request.expected_revision || current.manifest.version_id !== request.expected_version_id) {
        throw new Error("video.revision_conflict: The project changed before the image was selected.");
      }
      const receipt: VisualSourceReceipt & { consumed: boolean; localPath: string } = {
        id: `visual-receipt-${visualReceiptSequence++}`,
        receipt_kind: "user_selected",
        project_id: request.project_id,
        expected_revision: request.expected_revision,
        expected_version_id: request.expected_version_id,
        display_name: "editorial-glass-study.webp",
        sha256: "b".repeat(64),
        mime_type: "image/webp",
        size_bytes: 18_506,
        width: 640,
        height: 1_138,
        expires_at: "2099-01-01T00:00:00.000Z",
        consumed: false,
        localPath: FIXTURE_VISUAL_URL,
      };
      visualReceipts.set(receipt.id, receipt);
      const { consumed: _consumed, localPath: _localPath, ...publicReceipt } = receipt;
      return clone(publicReceipt);
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
      planned.manifest.caption_pages = makeCaptionPages(planned.manifest.scenes);
      planned.manifest.timeline = makeTimeline(planned.manifest.scenes, planned.manifest.source.duration_ms, planned.manifest.caption_pages);
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
      project.manifest.caption_pages = makeCaptionPages(project.manifest.scenes);
      project.manifest.timeline = makeTimeline(project.manifest.scenes, project.manifest.source.duration_ms, project.manifest.caption_pages);
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
    async editVideoTimeline(request) {
      const replay = timelineEditReplays.get(request.operation_id);
      if (replay) return clone({ ...replay, replayed: true });
      const current = projects.get(request.project_id);
      if (!current) throw new Error("Video project was not found.");
      if (current.revision !== request.expected_revision || current.manifest.version_id !== request.base_version_id) {
        throw new Error("video.revision_conflict: The project changed before the timeline edit could be applied.");
      }
      const edited = applyBrowserTimelineOperations(current, request);
      const response: VideoTimelineEditResponse = {
        project: store(edited),
        receipt: {
          project_id: request.project_id,
          expected_revision: request.expected_revision,
          base_version_id: request.base_version_id,
          operation_id: request.operation_id,
          changed_paths: ["reviewed_scenes", "tracks", "captions", "timeline_duration_us"],
          invalidated_stages: ["captions", "scene_render", "preview", "final_render", "publish_package"],
        },
        job_id: `timeline-edit-${request.operation_id}`,
        replayed: false,
      };
      timelineEditReplays.set(request.operation_id, clone(response));
      return clone(response);
    },
    async addVideoVisualAsset(request: AddVisualAssetRequest) {
      const replay = visualAssetReplays.get(request.operation_id);
      if (replay) return clone({ ...replay, replayed: true });
      const current = projects.get(request.project_id);
      if (!current) throw new Error("Video project was not found.");
      if (current.revision !== request.expected_revision || current.manifest.version_id !== request.expected_version_id) {
        throw new Error("video.revision_conflict: The project changed before the image could be added.");
      }
      if (request.origin.kind !== "user_selected") {
        throw new Error("video.approval_required: Choose the exact image through the file picker before adding it.");
      }
      const receipt = visualReceipts.get(request.origin.receipt_id);
      if (!receipt || receipt.consumed) {
        throw new Error("video.invalid_visual_receipt: The selected image receipt is missing, expired, or already used.");
      }
      if (receipt.project_id !== request.project_id
        || receipt.expected_revision !== request.expected_revision
        || receipt.expected_version_id !== request.expected_version_id) {
        throw new Error("video.revision_conflict: The selected image receipt belongs to a different project version.");
      }
      validateVisualRequest(request, current);
      const edited = clone(current);
      const priorVersion = edited.manifest.version_id;
      edited.revision += 1;
      edited.manifest.version_id = `${edited.id}-v${edited.revision + 1}`;
      const assetId = `visual-${request.operation_id}`;
      const layerId = `visual-layer-${request.operation_id}`;
      const visual: VideoVisualAsset = {
        id: assetId,
        mime_type: "image/webp",
        local_path: receipt.localPath,
        url: FIXTURE_VISUAL_URL,
        width: 640,
        height: 1_138,
        has_alpha: false,
        size_bytes: 18_506,
        checksum: "b".repeat(64),
        provenance: {
          kind: "user_upload",
          imported_at: FIXED_NOW,
          producer: "soundAr Video Studio",
          metadata: { operation_id: request.operation_id, display_name: receipt.display_name },
        },
        created_at: FIXED_NOW,
      };
      const layer: VideoVisualLayer = {
        id: layerId,
        asset_id: assetId,
        scene_id: request.scene_id,
        start_ms: request.range.start_us / 1_000,
        end_ms: request.range.end_us / 1_000,
        fit: request.fit,
        crop: request.crop,
        z_index: request.z_index,
        motion: clone(request.motion),
        transition_in_ms: request.transition_in_us / 1_000,
        transition_out_ms: request.transition_out_us / 1_000,
      };
      edited.manifest.visual_assets = [...(edited.manifest.visual_assets ?? []), visual];
      edited.manifest.visual_layers = [...(edited.manifest.visual_layers ?? []), layer];
      edited.manifest.timeline = makeTimeline(
        edited.manifest.scenes,
        edited.manifest.source.duration_ms,
        edited.manifest.caption_pages,
        edited.manifest.visual_layers,
        edited.manifest.visual_assets,
      );
      edited.manifest.artifacts = edited.manifest.artifacts.filter((artifact) => !["preview", "master", "variation", "publish-package"].includes(artifact.role));
      edited.manifest.revisions.push({ id: `revision-${edited.manifest.revisions.length + 1}`, created_at: FIXED_NOW, instruction: "Add a user-selected image layer.", affected_stages: ["preview", "export"], base_version_id: priorVersion, version_id: edited.manifest.version_id });
      edited.status = "editing";
      edited.master = undefined;
      edited.deliverables = [];
      edited.updated_at = FIXED_NOW;
      const response: AddVisualAssetResponse = {
        project: store(edited),
        asset_id: assetId,
        layer_id: layerId,
        job_id: `visual-job-${request.operation_id}`,
        replayed: false,
      };
      receipt.consumed = true;
      visualAssetReplays.set(request.operation_id, clone(response));
      return clone(response);
    },
    async reviseVideo(request) {
      const current = projects.get(request.project_id);
      if (!current) throw new Error("Video project was not found.");
      if (current.manifest.version_id !== request.base_version_id) throw new Error("The project changed. Refresh before applying this revision.");
      const revised = clone(current);
      const priorVersion = revised.manifest.version_id;
      revised.manifest.version_id = `${request.project_id}-v${revised.manifest.revisions.length + 2}`;
      revised.revision += 1;
      const requestedCaptionStyle = request.instruction.toLowerCase().includes("calm") ? "calm" : revised.manifest.settings.caption_style;
      revised.manifest.settings.caption_style = requestedCaptionStyle;
      revised.manifest.scenes = revised.manifest.scenes.map((scene) => scene.id === request.scene_id && request.scene_patch
        ? { ...scene, ...request.scene_patch }
        : request.scene_id ? scene : { ...scene, caption_style: requestedCaptionStyle });
      revised.manifest.caption_pages = makeCaptionPages(revised.manifest.scenes);
      revised.manifest.timeline = makeTimeline(
        revised.manifest.scenes,
        revised.manifest.source.duration_ms,
        revised.manifest.caption_pages,
        revised.manifest.visual_layers,
        revised.manifest.visual_assets,
      );
      revised.manifest.artifacts = revised.manifest.artifacts.filter((artifact) => !["preview", "master", "variation"].includes(artifact.role));
      revised.manifest.revisions.push({ id: `revision-${revised.manifest.revisions.length + 1}`, created_at: FIXED_NOW, instruction: request.instruction, affected_stages: ["preview", "export"], base_version_id: priorVersion, version_id: revised.manifest.version_id });
      revised.status = "editing";
      revised.master = undefined;
      revised.deliverables = [];
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
      const variations = Array.from({ length: Math.max(0, (request.variations ?? 1) - 1) }, (_, index): VideoArtifact => ({
        ...makeMasterArtifact(`${request.project_id}-variation-${index + 2}`, request.version_id, exported.duration_ms),
        id: `${request.project_id}-variation-${index + 2}`,
        project_id: request.project_id,
        role: "variation",
        title: `${exported.name} · Variation ${index + 2}`,
        download_name: `${request.project_id}-variation-${index + 2}.mp4`,
      }));
      exported.master = master;
      exported.deliverables = [master, ...variations];
      exported.status = "exported";
      exported.manifest.artifacts = [...exported.manifest.artifacts.filter((artifact) => !["master", "variation"].includes(artifact.role)), master, ...variations];
      return store(exported);
    },
    async exportPublishPackage(projectId) {
      const current = projects.get(projectId);
      if (!current?.master) throw new Error("Export a final master before creating a publish package.");
      const artifact: VideoArtifact = { id: `${projectId}-publish-package`, project_id: projectId, version_id: current.manifest.version_id, role: "publish-package", title: `${current.name} publish package`, mime_type: "application/zip", format: "zip", url: FIXTURE_PUBLISH_PACKAGE_URL, download_name: `${projectId}-publish-package.zip`, playable: false, created_at: FIXED_NOW };
      current.manifest.artifacts.push(artifact);
      current.deliverables = [...(current.deliverables ?? (current.master ? [current.master] : [])), artifact];
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
  const deliverables = project.deliverables?.map(withNativeArtifactUrl);
  return {
    ...project,
    master,
    deliverables,
    manifest: {
      ...project.manifest,
      source: project.manifest.source.preview_url || !project.manifest.source.local_path
        ? project.manifest.source
        : { ...project.manifest.source, preview_url: convertFileSrc(project.manifest.source.local_path) },
      visual_assets: project.manifest.visual_assets?.map((asset) => ({
        ...asset,
        url: asset.url || !asset.local_path ? asset.url : convertFileSrc(asset.local_path),
      })),
      artifacts,
    },
  };
}

function withNativeSummaryUrls(project: VideoProjectSummary): VideoProjectSummary {
  return {
    ...project,
    master: project.master ? withNativeArtifactUrl(project.master) : undefined,
    deliverables: project.deliverables?.map(withNativeArtifactUrl),
  };
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
    chooseVideoVisualAsset: (request) => invoke<VisualSourceReceipt | null>("choose_video_visual_asset", { request }),
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
    editVideoTimeline: (request) => invoke<VideoTimelineEditResponse>("edit_video_timeline", { request }).then((response) => ({ ...response, project: withNativeProjectUrls(response.project) })),
    addVideoVisualAsset: (request) => invoke<AddVisualAssetResponse>("add_video_visual_asset", { request }).then((response) => ({ ...response, project: withNativeProjectUrls(response.project) })),
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
