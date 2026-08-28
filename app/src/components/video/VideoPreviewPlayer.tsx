import { Maximize2, Minus, Pause, Play, Plus, SkipBack, SkipForward, Volume2 } from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import type { VideoArtifact, VideoCanvasBounds, VideoCaptionPage, VideoScene, VideoTranscriptSegment, VideoVisualAsset, VideoVisualLayer } from "../../types/video";
import { formatVideoClock } from "../../lib/videoState";
import { videoSourceWithFirstFrame } from "../../lib/videoPlayback";

export function VideoPreviewPlayer({
  sourceUrl,
  artifact,
  scene,
  scenes = [],
  transcript = [],
  captionPages,
  visualAssets = [],
  visualLayers = [],
  projectDurationMs,
  playheadMs,
  onPlayheadChange,
  onSelectCaption,
  onCaptionBoundsChange,
  selectedVisualLayerId,
  onSelectVisual,
  onVisualBoundsChange,
}: {
  sourceUrl?: string;
  artifact?: VideoArtifact;
  scene?: VideoScene;
  scenes?: VideoScene[];
  transcript?: VideoTranscriptSegment[];
  captionPages?: VideoCaptionPage[];
  visualAssets?: VideoVisualAsset[];
  visualLayers?: VideoVisualLayer[];
  projectDurationMs: number;
  playheadMs: number;
  onPlayheadChange: (milliseconds: number) => void;
  onSelectCaption?: () => void;
  onCaptionBoundsChange?: (bounds: VideoCanvasBounds) => void;
  selectedVisualLayerId?: string;
  onSelectVisual?: (layerId: string) => void;
  onVisualBoundsChange?: (layerId: string, bounds: VideoCanvasBounds) => void;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const progressCallbackRef = useRef(onPlayheadChange);
  const [playing, setPlaying] = useState(false);
  const [gapPlayback, setGapPlayback] = useState<{ startMs: number; endMs: number; startedAt: number; nextScene?: VideoScene }>();
  const [volume, setVolume] = useState(0.72);
  const [aspectRatio, setAspectRatio] = useState("9 / 16");
  const [zoom, setZoom] = useState<"fit" | 50 | 75 | 100>("fit");
  const [captionSelected, setCaptionSelected] = useState(false);
  const [previewCaptionBounds, setPreviewCaptionBounds] = useState<VideoCanvasBounds>(DEFAULT_CAPTION_BOUNDS);

  progressCallbackRef.current = onPlayheadChange;
  const timelineScenes = scenes.length
    ? scenes.map((candidate) => candidate.id === scene?.id ? scene : candidate).sort((left, right) => left.timeline_start_ms - right.timeline_start_ms)
    : scene ? [scene] : [];
  const mapping = mapTimelineToSource(timelineScenes, playheadMs, projectDurationMs);
  const activeScene = mapping.kind === "scene" ? mapping.scene : undefined;
  const mediaMilliseconds = artifact
    ? playheadMs
    : mapping.kind === "scene"
      ? mapping.sourceMs
      : mapping.previousScene?.source_end_ms ?? mapping.nextScene?.source_start_ms ?? playheadMs;
  const authoritativeCaptionPage = !artifact && activeScene?.captions_enabled && captionPages
    ? activeCaptionPage(captionPages, playheadMs, activeScene.id)
    : undefined;
  const fallbackCaptionCue = !artifact && activeScene?.captions_enabled && captionPages === undefined
    ? activeCaptionCue(activeScene, transcript, playheadMs)
    : undefined;
  const captionCue = authoritativeCaptionPage?.text ?? fallbackCaptionCue;
  const captionStyle = authoritativeCaptionPage?.style_id ?? activeScene?.caption_style;
  const projectedCaptionBounds = authoritativeCaptionPage?.bounds ?? activeScene?.caption_bounds ?? DEFAULT_CAPTION_BOUNDS;
  const activeVisuals = artifact ? [] : projectVisualLayers(visualAssets, visualLayers, playheadMs);
  const previousSceneStartMs = mapping.kind === "gap"
    ? mapping.previousScene?.timeline_start_ms ?? 0
    : activeScene?.timeline_start_ms ?? 0;

  useEffect(() => {
    if (videoRef.current && Math.abs(videoRef.current.currentTime * 1000 - mediaMilliseconds) > 1_100) {
      const durationSeconds = Number.isFinite(videoRef.current.duration) ? videoRef.current.duration : mediaMilliseconds / 1000;
      videoRef.current.currentTime = Math.min(durationSeconds, mediaMilliseconds / 1000);
    }
  }, [mediaMilliseconds]);

  useEffect(() => {
    if (!artifact && mapping.kind === "gap" && !gapPlayback) videoRef.current?.pause();
  }, [artifact, gapPlayback, mapping.kind, playheadMs]);

  useEffect(() => {
    if (!gapPlayback) return;
    let frame = 0;
    const tick = (now: number) => {
      const nextMs = Math.min(gapPlayback.endMs, gapPlayback.startMs + (now - gapPlayback.startedAt));
      progressCallbackRef.current(nextMs);
      if (nextMs < gapPlayback.endMs) {
        frame = requestAnimationFrame(tick);
        return;
      }
      const nextScene = gapPlayback.nextScene;
      setGapPlayback(undefined);
      if (!nextScene || !videoRef.current) {
        setPlaying(false);
        return;
      }
      videoRef.current.currentTime = nextScene.source_start_ms / 1000;
      void videoRef.current.play().catch(() => setPlaying(false));
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [gapPlayback]);

  useEffect(() => {
    if (!activeScene && !scene) return;
    const layout = activeScene?.layout ?? scene!.layout;
    setAspectRatio(layout === "landscape" ? "16 / 9" : layout === "square" ? "1" : "9 / 16");
  }, [activeScene?.id, activeScene?.layout, scene?.id, scene?.layout]);

  useEffect(() => {
    if (!videoRef.current) return;
    videoRef.current.volume = volume;
    videoRef.current.muted = volume === 0;
  }, [volume]);

  useEffect(() => {
    setPreviewCaptionBounds(projectedCaptionBounds);
  }, [projectedCaptionBounds.x_bp, projectedCaptionBounds.y_bp, projectedCaptionBounds.width_bp, projectedCaptionBounds.height_bp]);

  async function togglePlayback() {
    const video = videoRef.current;
    if (!video) return;
    try {
      if (gapPlayback) {
        setGapPlayback(undefined);
        setPlaying(false);
      } else if (!artifact && mapping.kind === "gap") {
        beginGapPlayback(mapping);
      } else if (video.paused) await video.play(); else video.pause();
    } catch {
      setPlaying(false);
    }
  }

  function beginGapPlayback(gap: Extract<TimelineSourceMapping, { kind: "gap" }>) {
    const startMs = Math.max(gap.startMs, playheadMs);
    if (gap.endMs <= startMs || !gap.nextScene) {
      setPlaying(false);
      return;
    }
    videoRef.current?.pause();
    setPlaying(true);
    setGapPlayback({ startMs, endMs: gap.endMs, startedAt: performance.now(), nextScene: gap.nextScene });
  }

  function advanceProxyPlayback(currentSourceMs: number) {
    if (artifact || !activeScene) return;
    const timelineMs = mapSourceToTimeline(activeScene, currentSourceMs);
    if (currentSourceMs < activeScene.source_end_ms - 20) {
      onPlayheadChange(timelineMs);
      return;
    }
    const nextScene = timelineScenes.find((candidate) => candidate.timeline_start_ms >= activeScene.timeline_end_ms && candidate.id !== activeScene.id);
    onPlayheadChange(activeScene.timeline_end_ms);
    if (!nextScene) {
      videoRef.current?.pause();
      setPlaying(false);
      return;
    }
    if (nextScene.timeline_start_ms > activeScene.timeline_end_ms) {
      beginGapPlayback({ kind: "gap", startMs: activeScene.timeline_end_ms, endMs: nextScene.timeline_start_ms, previousScene: activeScene, nextScene });
      return;
    }
    if (videoRef.current) {
      videoRef.current.currentTime = nextScene.source_start_ms / 1000;
      void videoRef.current.play().catch(() => setPlaying(false));
    }
  }

  function seek(milliseconds: number) {
    onPlayheadChange(Math.max(0, Math.min(projectDurationMs, milliseconds)));
  }

  async function enterFullscreen() {
    const frame = videoRef.current?.parentElement;
    if (frame?.requestFullscreen) await frame.requestFullscreen();
  }

  function commitCaptionBounds(bounds: VideoCanvasBounds) {
    const next = canonicalCaptionBounds(bounds);
    setPreviewCaptionBounds(next);
    onCaptionBoundsChange?.(next);
  }

  function moveCaptionWithKeyboard(event: KeyboardEvent<HTMLButtonElement>) {
    if (!onCaptionBoundsChange || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const delta = event.shiftKey ? 500 : 100;
    const horizontal = event.key === "ArrowLeft" ? -delta : event.key === "ArrowRight" ? delta : 0;
    const vertical = event.key === "ArrowUp" ? -delta : event.key === "ArrowDown" ? delta : 0;
    commitCaptionBounds(event.altKey
      ? { ...previewCaptionBounds, width_bp: previewCaptionBounds.width_bp + horizontal, height_bp: previewCaptionBounds.height_bp + vertical }
      : { ...previewCaptionBounds, x_bp: previewCaptionBounds.x_bp + horizontal, y_bp: previewCaptionBounds.y_bp + vertical });
  }

  function beginCaptionPointerEdit(event: ReactPointerEvent<HTMLButtonElement>) {
    if (!onCaptionBoundsChange) return;
    event.preventDefault();
    event.stopPropagation();
    setCaptionSelected(true);
    onSelectCaption?.();
    const frame = event.currentTarget.parentElement?.getBoundingClientRect();
    if (!frame?.width || !frame.height) return;
    const startX = event.clientX;
    const startY = event.clientY;
    const start = previewCaptionBounds;
    const handle = (event.target as HTMLElement).dataset.captionHandle as CaptionHandle | undefined;
    let latest = start;
    const move = (pointer: PointerEvent) => {
      const deltaX = Math.round((pointer.clientX - startX) / frame.width * 10_000);
      const deltaY = Math.round((pointer.clientY - startY) / frame.height * 10_000);
      latest = handle ? resizeCaptionBounds(start, handle, deltaX, deltaY) : canonicalCaptionBounds({ ...start, x_bp: start.x_bp + deltaX, y_bp: start.y_bp + deltaY });
      setPreviewCaptionBounds(latest);
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      if (JSON.stringify(latest) !== JSON.stringify(start)) onCaptionBoundsChange(latest);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  function moveVisualWithKeyboard(layerId: string, bounds: VideoCanvasBounds, event: KeyboardEvent<HTMLButtonElement>) {
    if (!onVisualBoundsChange || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const delta = event.shiftKey ? 500 : 100;
    const horizontal = event.key === "ArrowLeft" ? -delta : event.key === "ArrowRight" ? delta : 0;
    const vertical = event.key === "ArrowUp" ? -delta : event.key === "ArrowDown" ? delta : 0;
    const next = event.altKey
      ? resizeVisualBounds(bounds, "se", horizontal || vertical, horizontal || vertical)
      : canonicalVisualBounds({ ...bounds, x_bp: bounds.x_bp + horizontal, y_bp: bounds.y_bp + vertical });
    if (JSON.stringify(next) !== JSON.stringify(bounds)) onVisualBoundsChange(layerId, next);
  }

  function beginVisualPointerEdit(layerId: string, bounds: VideoCanvasBounds, event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();
    event.stopPropagation();
    onSelectVisual?.(layerId);
    if (!onVisualBoundsChange) return;
    const frame = event.currentTarget.parentElement?.getBoundingClientRect();
    if (!frame?.width || !frame.height) return;
    const startX = event.clientX;
    const startY = event.clientY;
    const target = event.currentTarget;
    const handle = (event.target as HTMLElement).dataset.visualHandle as VisualHandle | undefined;
    let latest = bounds;
    const move = (pointer: PointerEvent) => {
      const deltaX = Math.round((pointer.clientX - startX) / frame.width * 10_000);
      const deltaY = Math.round((pointer.clientY - startY) / frame.height * 10_000);
      latest = handle
        ? resizeVisualBounds(bounds, handle, deltaX, deltaY)
        : canonicalVisualBounds({ ...bounds, x_bp: bounds.x_bp + deltaX, y_bp: bounds.y_bp + deltaY });
      target.style.left = `${latest.x_bp / 100}%`;
      target.style.top = `${latest.y_bp / 100}%`;
      target.style.width = `${latest.width_bp / 100}%`;
      target.style.height = `${latest.height_bp / 100}%`;
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      if (JSON.stringify(latest) !== JSON.stringify(bounds)) onVisualBoundsChange(layerId, latest);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  const captionGeometryStyle = {
    left: `${previewCaptionBounds.x_bp / 100}%`,
    top: `${previewCaptionBounds.y_bp / 100}%`,
    right: "auto",
    bottom: "auto",
    width: `${previewCaptionBounds.width_bp / 100}%`,
    height: `${previewCaptionBounds.height_bp / 100}%`,
    "--video-caption-font-size": authoritativeCaptionPage?.font_size_bp ?? 480,
  } as CSSProperties;

  return (
    <section className="video-preview-player" aria-label="Fast video preview">
      <header><select aria-label="Preview aspect ratio" value={aspectRatio} onChange={(event) => setAspectRatio(event.target.value)}><option value="9 / 16">9:16 Portrait</option><option value="16 / 9">16:9 Landscape</option><option value="1">1:1 Square</option></select><div className="video-preview-zoom" role="group" aria-label="Canvas zoom"><button type="button" aria-label="Zoom canvas out" disabled={zoom === 50} onClick={() => setZoom((value) => value === "fit" || value === 100 ? 75 : 50)}><Minus aria-hidden="true" size={12} /></button><button type="button" aria-label="Fit canvas" aria-pressed={zoom === "fit"} onClick={() => setZoom("fit")}>{zoom === "fit" ? "Fit" : `${zoom}%`}</button><button type="button" aria-label="Zoom canvas in" disabled={zoom === 100} onClick={() => setZoom((value) => value === 50 ? 75 : 100)}><Plus aria-hidden="true" size={12} /></button></div><span>{artifact ? "Rendered preview" : "Low-resolution proxy"}</span></header>
      <div className="video-preview-stage">
        <div className="video-portrait-frame" style={{ aspectRatio, height: zoom === "fit" ? undefined : `${zoom}%` }} onDoubleClick={() => void enterFullscreen()}>
          <video ref={videoRef} src={videoSourceWithFirstFrame(artifact?.url ?? sourceUrl)} poster={artifact?.poster_url} playsInline preload="auto" aria-label={artifact?.title ?? "Project proxy preview"} onPlay={() => setPlaying(true)} onPause={() => { if (!gapPlayback) setPlaying(false); }} onTimeUpdate={(event) => {
            if (!playing) return;
            const currentMs = event.currentTarget.currentTime * 1000;
            if (artifact) onPlayheadChange(Math.max(0, Math.min(projectDurationMs, currentMs)));
            else advanceProxyPlayback(currentMs);
          }} />
          {!artifact && mapping.kind === "gap" ? <div className="video-preview-gap" role="status"><span>Preserved source gap</span><small>{formatVideoClock(mapping.endMs - mapping.startMs)} of silence</small></div> : null}
          {activeVisuals.map(({ asset, layer, bounds, opacity }, index) => asset.url ? <button
            key={layer.id}
            className={`video-visual-preview-layer ${selectedVisualLayerId === layer.id ? "is-selected" : ""}`}
            type="button"
            data-visual-layer-id={layer.id}
            aria-label={`Select image layer ${index + 1}`}
            aria-pressed={selectedVisualLayerId === layer.id}
            aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight"
            title="Drag to position. Drag a corner to resize. Arrow keys move; Alt+Arrow resizes."
            style={{
              left: `${bounds.x_bp / 100}%`,
              top: `${bounds.y_bp / 100}%`,
              width: `${bounds.width_bp / 100}%`,
              height: `${bounds.height_bp / 100}%`,
              opacity,
              zIndex: index + 1,
            }}
            onPointerDown={(event) => beginVisualPointerEdit(layer.id, bounds, event)}
            onKeyDown={(event) => moveVisualWithKeyboard(layer.id, bounds, event)}
            onClick={() => onSelectVisual?.(layer.id)}
          ><img src={asset.url} alt="" style={{ objectFit: layer.fit === "stretch" ? "fill" : layer.fit }} />{selectedVisualLayerId === layer.id ? <span className="video-visual-selection-handles" aria-hidden="true"><i data-visual-handle="nw" /><i data-visual-handle="ne" /><i data-visual-handle="se" /><i data-visual-handle="sw" /></span> : null}</button> : null)}
          {captionCue && activeScene && captionStyle ? <button className={`video-caption-preview has-geometry is-${captionStyle} ${captionSelected ? "is-selected" : ""}`} type="button" style={captionGeometryStyle} data-caption-page-id={authoritativeCaptionPage?.id} data-editor-selection="caption" aria-label={`Select active caption: ${captionCue}`} aria-pressed={captionSelected} aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight" title="Drag to position. Drag a corner to resize. Arrow keys move; Alt+Arrow resizes; Shift uses larger steps. Save scene changes to persist." onPointerDown={beginCaptionPointerEdit} onKeyDown={moveCaptionWithKeyboard} onDoubleClick={(event) => event.stopPropagation()} onClick={() => { setCaptionSelected(true); onSelectCaption?.(); }}>{captionCue}{captionSelected ? <span className="video-caption-selection-handles" aria-hidden="true"><i data-caption-handle="nw" /><i data-caption-handle="ne" /><i data-caption-handle="se" /><i data-caption-handle="sw" /></span> : null}</button> : null}
        </div>
      </div>
      <footer className="video-preview-controls">
        <div><button className="video-icon-button" type="button" aria-label="Previous scene" onClick={() => seek(previousSceneStartMs)}><SkipBack aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label={playing || gapPlayback ? "Pause preview" : "Play preview"} onClick={() => void togglePlayback()}>{playing || gapPlayback ? <Pause aria-hidden="true" size={15} /> : <Play aria-hidden="true" size={15} />}</button><button className="video-icon-button" type="button" aria-label="Next scene" onClick={() => seek(mapping.kind === "gap" ? mapping.nextScene?.timeline_start_ms ?? projectDurationMs : timelineScenes.find((candidate) => candidate.timeline_start_ms >= (activeScene?.timeline_end_ms ?? playheadMs))?.timeline_start_ms ?? projectDurationMs)}><SkipForward aria-hidden="true" size={14} /></button></div>
        <strong>{formatVideoClock(playheadMs, true)} <span>/ {formatVideoClock(projectDurationMs, true)}</span></strong>
        <div><Volume2 aria-hidden="true" size={14} /><input aria-label="Preview volume" type="range" min={0} max={1} step={0.05} value={volume} onChange={(event) => setVolume(Number(event.target.value))} /><button className="video-icon-button" type="button" aria-label="Full screen preview" onClick={() => void enterFullscreen()}><Maximize2 aria-hidden="true" size={14} /></button></div>
      </footer>
    </section>
  );
}

const DEFAULT_CAPTION_BOUNDS: VideoCanvasBounds = { x_bp: 800, y_bp: 7350, width_bp: 8400, height_bp: 1500 };
type CaptionHandle = "nw" | "ne" | "se" | "sw";
type VisualHandle = CaptionHandle;

function canonicalCaptionBounds(bounds: VideoCanvasBounds): VideoCanvasBounds {
  const width = Math.max(1_600, Math.min(10_000, Math.round(bounds.width_bp)));
  const height = Math.max(600, Math.min(10_000, Math.round(bounds.height_bp)));
  return {
    x_bp: Math.max(0, Math.min(10_000 - width, Math.round(bounds.x_bp))),
    y_bp: Math.max(0, Math.min(10_000 - height, Math.round(bounds.y_bp))),
    width_bp: width,
    height_bp: height,
  };
}

function resizeCaptionBounds(start: VideoCanvasBounds, handle: CaptionHandle, deltaX: number, deltaY: number): VideoCanvasBounds {
  const movesLeft = handle === "nw" || handle === "sw";
  const movesTop = handle === "nw" || handle === "ne";
  const proposed = {
    x_bp: movesLeft ? start.x_bp + deltaX : start.x_bp,
    y_bp: movesTop ? start.y_bp + deltaY : start.y_bp,
    width_bp: start.width_bp + (movesLeft ? -deltaX : deltaX),
    height_bp: start.height_bp + (movesTop ? -deltaY : deltaY),
  };
  return canonicalCaptionBounds(proposed);
}

function canonicalVisualBounds(bounds: VideoCanvasBounds): VideoCanvasBounds {
  const width = Math.max(500, Math.min(10_000, Math.round(bounds.width_bp)));
  const height = Math.max(500, Math.min(10_000, Math.round(bounds.height_bp)));
  return {
    x_bp: Math.max(0, Math.min(10_000 - width, Math.round(bounds.x_bp))),
    y_bp: Math.max(0, Math.min(10_000 - height, Math.round(bounds.y_bp))),
    width_bp: width,
    height_bp: height,
  };
}

function resizeVisualBounds(start: VideoCanvasBounds, handle: VisualHandle, deltaX: number, deltaY: number): VideoCanvasBounds {
  const growsRight = handle === "ne" || handle === "se";
  const growsDown = handle === "se" || handle === "sw";
  const aspect = start.width_bp / Math.max(1, start.height_bp);
  const horizontalGrowth = growsRight ? deltaX : -deltaX;
  const verticalGrowth = (growsDown ? deltaY : -deltaY) * aspect;
  const growth = Math.abs(deltaX) >= Math.abs(deltaY) ? horizontalGrowth : verticalGrowth;
  let width = Math.max(500, start.width_bp + growth);
  let height = Math.max(500, Math.round(width / aspect));
  const maxWidth = (handle === "nw" || handle === "sw") ? start.x_bp + start.width_bp : 10_000 - start.x_bp;
  const maxHeight = (handle === "nw" || handle === "ne") ? start.y_bp + start.height_bp : 10_000 - start.y_bp;
  const scale = Math.min(1, maxWidth / width, maxHeight / height);
  width = Math.max(500, Math.round(width * scale));
  height = Math.max(500, Math.round(height * scale));
  return canonicalVisualBounds({
    x_bp: handle === "nw" || handle === "sw" ? start.x_bp + start.width_bp - width : start.x_bp,
    y_bp: handle === "nw" || handle === "ne" ? start.y_bp + start.height_bp - height : start.y_bp,
    width_bp: width,
    height_bp: height,
  });
}

export type TimelineSourceMapping =
  | { kind: "scene"; scene: VideoScene; sourceMs: number }
  | { kind: "gap"; startMs: number; endMs: number; previousScene?: VideoScene; nextScene?: VideoScene };

export function mapTimelineToSource(scenes: VideoScene[], playheadMs: number, projectDurationMs: number): TimelineSourceMapping {
  const ordered = [...scenes].sort((left, right) => left.timeline_start_ms - right.timeline_start_ms);
  const safePlayhead = Math.max(0, Math.min(projectDurationMs, playheadMs));
  const scene = ordered.find((candidate, index) => safePlayhead >= candidate.timeline_start_ms && (
    safePlayhead < candidate.timeline_end_ms || (index === ordered.length - 1 && safePlayhead === candidate.timeline_end_ms)
  ));
  if (scene) {
    const sceneDuration = Math.max(0, scene.timeline_end_ms - scene.timeline_start_ms);
    const sourceDuration = Math.max(0, scene.source_end_ms - scene.source_start_ms);
    const elapsed = Math.max(0, Math.min(sceneDuration, safePlayhead - scene.timeline_start_ms));
    const sourceElapsed = sceneDuration > 0 ? elapsed * sourceDuration / sceneDuration : 0;
    return { kind: "scene", scene, sourceMs: scene.source_start_ms + Math.min(sourceDuration, sourceElapsed) };
  }
  const nextIndex = ordered.findIndex((candidate) => candidate.timeline_start_ms > safePlayhead);
  const nextScene = nextIndex >= 0 ? ordered[nextIndex] : undefined;
  const previousScene = nextIndex > 0 ? ordered[nextIndex - 1] : nextIndex < 0 ? ordered.at(-1) : undefined;
  return {
    kind: "gap",
    startMs: previousScene?.timeline_end_ms ?? 0,
    endMs: nextScene?.timeline_start_ms ?? projectDurationMs,
    previousScene,
    nextScene,
  };
}

export function mapSourceToTimeline(scene: VideoScene, sourceMs: number): number {
  const timelineDuration = Math.max(0, scene.timeline_end_ms - scene.timeline_start_ms);
  const sourceDuration = Math.max(0, scene.source_end_ms - scene.source_start_ms);
  const sourceElapsed = Math.max(0, Math.min(sourceDuration, sourceMs - scene.source_start_ms));
  const timelineElapsed = sourceDuration > 0 ? sourceElapsed * timelineDuration / sourceDuration : 0;
  return scene.timeline_start_ms + Math.min(timelineDuration, timelineElapsed);
}

export function activeCaptionPage(
  pages: VideoCaptionPage[],
  playheadMs: number,
  sceneId?: string,
): VideoCaptionPage | undefined {
  const matching = pages
    .filter((page) => !sceneId || !page.scene_id || page.scene_id === sceneId)
    .sort((left, right) => left.start_ms - right.start_ms || left.end_ms - right.end_ms);
  return matching.find((page, index) => playheadMs >= page.start_ms && (
    playheadMs < page.end_ms || (index === matching.length - 1 && playheadMs === page.end_ms)
  ));
}

export function activeCaptionCue(
  scene: VideoScene,
  transcript: VideoTranscriptSegment[],
  playheadMs: number,
): string {
  // Migration-era manifests do not contain renderer-authored caption pages. Keep
  // this deterministic paging path only until those projects are re-saved.
  const sceneDuration = Math.max(1, scene.timeline_end_ms - scene.timeline_start_ms);
  const elapsed = Math.max(0, Math.min(sceneDuration - 1, playheadMs - scene.timeline_start_ms));
  const mapping = mapTimelineToSource([scene], playheadMs, scene.timeline_end_ms);
  const sourceMs = mapping.kind === "scene" ? mapping.sourceMs : scene.source_start_ms;
  const segments = transcript.filter((segment) => (
    segment.end_ms > scene.source_start_ms && segment.start_ms < scene.source_end_ms
  ));
  const segment = segments.find((candidate) => sourceMs >= candidate.start_ms && sourceMs < candidate.end_ms)
    ?? segments.find((candidate) => sourceMs < candidate.start_ms)
    ?? segments.at(-1);
  if (segment) {
    const segmentDuration = Math.max(1, segment.end_ms - segment.start_ms);
    return captionPage(segment.text, (sourceMs - segment.start_ms) / segmentDuration);
  }
  return captionPage(scene.transcript, elapsed / sceneDuration);
}

function captionPage(text: string, progress: number): string {
  const words = text.trim().split(/\s+/).filter(Boolean);
  if (!words.length) return "";
  const wordsPerPage = 8;
  const pageCount = Math.ceil(words.length / wordsPerPage);
  const page = Math.min(pageCount - 1, Math.max(0, Math.floor(Math.max(0, Math.min(.999, progress)) * pageCount)));
  return words.slice(page * wordsPerPage, (page + 1) * wordsPerPage).join(" ");
}

export function projectVisualLayers(
  assets: VideoVisualAsset[],
  layers: VideoVisualLayer[],
  playheadMs: number,
): Array<{ asset: VideoVisualAsset; layer: VideoVisualLayer; bounds: VideoCanvasBounds; opacity: number }> {
  const assetById = new Map(assets.map((asset) => [asset.id, asset]));
  return layers
    .filter((layer) => playheadMs >= layer.start_ms && playheadMs <= layer.end_ms && assetById.has(layer.asset_id))
    .sort((left, right) => left.z_index - right.z_index || left.id.localeCompare(right.id))
    .map((layer) => {
      const duration = Math.max(1, layer.end_ms - layer.start_ms);
      const linearProgress = Math.max(0, Math.min(1, (playheadMs - layer.start_ms) / duration));
      const progress = layer.motion.easing === "ease_in_out"
        ? linearProgress * linearProgress * (3 - 2 * linearProgress)
        : linearProgress;
      const interpolate = (start: number, end: number) => Math.round(start + (end - start) * progress);
      const fadeIn = layer.transition_in_ms > 0 ? Math.min(1, (playheadMs - layer.start_ms) / layer.transition_in_ms) : 1;
      const fadeOut = layer.transition_out_ms > 0 ? Math.min(1, (layer.end_ms - playheadMs) / layer.transition_out_ms) : 1;
      return {
        asset: assetById.get(layer.asset_id)!,
        layer,
        bounds: {
          x_bp: interpolate(layer.motion.start_bounds.x_bp, layer.motion.end_bounds.x_bp),
          y_bp: interpolate(layer.motion.start_bounds.y_bp, layer.motion.end_bounds.y_bp),
          width_bp: interpolate(layer.motion.start_bounds.width_bp, layer.motion.end_bounds.width_bp),
          height_bp: interpolate(layer.motion.start_bounds.height_bp, layer.motion.end_bounds.height_bp),
        },
        opacity: Math.max(0, Math.min(1, layer.motion.start_opacity_milli / 1_000 * fadeIn * fadeOut)),
      };
    });
}
