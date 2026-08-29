import { Captions, GripHorizontal, Image as ImageIcon, Minus, Music2, Plus, Scissors, Video, Volume2, Waves } from "lucide-react";
import { useState, type CSSProperties, type KeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import type { VideoScene, VideoTimelineItem, VideoTimelineManifest, VideoTimelineOperation, VideoTimelineTrackKind } from "../../types/video";
import { formatVideoClock } from "../../lib/videoState";

const trackMeta: Record<VideoTimelineTrackKind, { label: string; icon: typeof Video }> = {
  video: { label: "Video", icon: Video },
  visuals: { label: "Visuals", icon: ImageIcon },
  captions: { label: "Captions", icon: Captions },
  voice: { label: "Voice", icon: Waves },
  music: { label: "Music", icon: Music2 },
};
const baseTrackOrder: VideoTimelineTrackKind[] = ["video", "captions", "voice", "music"];
export type VideoTimelineMode = "collapsed" | "compact" | "expanded";
type TimelineGesture = {
  itemId: string;
  kind: "reorder" | "trim-start" | "trim-end";
  deltaMs: number;
  deltaPx: number;
};

function itemStyle(start: number, end: number, duration: number): CSSProperties {
  return {
    left: `${duration ? (start / duration) * 100 : 0}%`,
    width: `${duration ? Math.max(0.7, ((end - start) / duration) * 100) : 0}%`,
  };
}

export function VideoTimeline({
  timeline,
  scenes,
  playheadMs,
  selectedSceneId,
  onPlayheadChange,
  onSelectScene,
  onEditTimeline,
  editing = false,
  height,
  onHeightChange,
  mode = "compact",
  onModeChange,
  onTransportHost,
}: {
  timeline: VideoTimelineManifest;
  scenes: VideoScene[];
  playheadMs: number;
  selectedSceneId?: string;
  onPlayheadChange: (milliseconds: number) => void;
  onSelectScene: (sceneId: string) => void;
  onEditTimeline?: (operations: VideoTimelineOperation[], label: string) => Promise<void> | void;
  editing?: boolean;
  height?: number;
  onHeightChange?: (height: number) => void;
  mode?: VideoTimelineMode;
  onModeChange?: (mode: VideoTimelineMode) => void;
  /** Receives the toolbar slot the preview transport is rendered into. */
  onTransportHost?: (element: HTMLDivElement | null) => void;
}) {
  const duration = Math.max(1, timeline.duration_ms);
  const [zoom, setZoom] = useState(1);
  const [selectedTrack, setSelectedTrack] = useState<VideoTimelineTrackKind>("video");
  const [gesture, setGesture] = useState<TimelineGesture>();
  const trackOrder = timeline.tracks.some((track) => track.kind === "visuals")
    ? ["video", "visuals", "captions", "voice", "music"] satisfies VideoTimelineTrackKind[]
    : baseTrackOrder;
  const tickCount = Math.max(2, Math.ceil(duration / 15_000));
  const ticks = Array.from({ length: tickCount + 1 }, (_, index) => Math.min(duration, index * 15_000));

  function movePlayhead(event: KeyboardEvent<HTMLInputElement>) {
    const step = event.shiftKey ? 5_000 : 1_000;
    const next = event.key === "ArrowRight" || event.key === "ArrowUp"
      ? playheadMs + step
      : event.key === "ArrowLeft" || event.key === "ArrowDown"
        ? playheadMs - step
        : event.key === "Home"
          ? 0
          : event.key === "End"
            ? duration
            : undefined;
    if (next === undefined) return;
    event.preventDefault();
    onPlayheadChange(Math.max(0, Math.min(duration, next)));
  }

  function beginHeightResize(event: ReactPointerEvent<HTMLDivElement>) {
    if (!onHeightChange) return;
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = height ?? 210;
    const move = (pointer: PointerEvent) => onHeightChange(clampTimelineHeight(startHeight + startY - pointer.clientY));
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  function resizeHeightWithKeyboard(event: KeyboardEvent<HTMLDivElement>) {
    if (!onHeightChange || !["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    if (onModeChange) {
      const modes: VideoTimelineMode[] = ["collapsed", "compact", "expanded"];
      const current = modes.indexOf(mode);
      const nextMode = event.key === "Home" ? "collapsed" : event.key === "End" ? "expanded" : modes[Math.max(0, Math.min(modes.length - 1, current + (event.key === "ArrowUp" ? 1 : -1)))];
      onModeChange(nextMode);
      return;
    }
    const next = event.key === "Home" ? 210 : event.key === "End" ? 420 : (height ?? 210) + (event.key === "ArrowUp" ? 20 : -20);
    onHeightChange(clampTimelineHeight(next));
  }

  function commit(operations: VideoTimelineOperation[], label: string) {
    if (!onEditTimeline || editing) return;
    void onEditTimeline(operations, label);
  }

  function splitSelectedScene() {
    if (!selectedSceneId) return;
    commit([{ type: "split_scene", scene_id: selectedSceneId, at_timeline_us: millisecondsToMicroseconds(playheadMs) }], "Split scene");
  }

  function beginReorder(item: VideoTimelineItem, event: ReactPointerEvent<HTMLButtonElement>) {
    if (!onEditTimeline || editing || item.track !== "video" || !item.scene_id || event.button !== 0) return;
    const lane = event.currentTarget.parentElement?.parentElement;
    const fromIndex = scenes.findIndex((scene) => scene.id === item.scene_id);
    if (!lane || fromIndex < 0) return;
    const rect = lane.getBoundingClientRect();
    const startX = event.clientX;
    let toIndex = fromIndex;
    let moved = false;
    const move = (pointer: PointerEvent) => {
      const deltaPx = pointer.clientX - startX;
      moved ||= Math.abs(deltaPx) >= 4;
      if (!moved) return;
      const projectMs = Math.max(0, Math.min(duration, ((pointer.clientX - rect.left) / Math.max(1, rect.width)) * duration));
      const midpointIndex = scenes.findIndex((scene) => projectMs < (scene.timeline_start_ms + scene.timeline_end_ms) / 2);
      toIndex = midpointIndex < 0 ? scenes.length - 1 : midpointIndex;
      setGesture({ itemId: item.id, kind: "reorder", deltaMs: 0, deltaPx });
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      setGesture(undefined);
      if (moved && toIndex !== fromIndex) commit([{ type: "reorder_scene", scene_id: item.scene_id!, to_index: toIndex }], `Move ${item.label}`);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  function beginTrim(scene: VideoScene, item: VideoTimelineItem, edge: "start" | "end", event: ReactPointerEvent<HTMLSpanElement>) {
    if (!onEditTimeline || editing || event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const lane = event.currentTarget.parentElement?.parentElement;
    if (!lane) return;
    const startX = event.clientX;
    const rect = lane.getBoundingClientRect();
    const sceneDuration = scene.timeline_end_ms - scene.timeline_start_ms;
    let deltaMs = 0;
    const move = (pointer: PointerEvent) => {
      const raw = snapMilliseconds(((pointer.clientX - startX) / Math.max(1, rect.width)) * duration);
      deltaMs = edge === "start"
        ? Math.max(0, Math.min(sceneDuration - 100, raw))
        : Math.min(0, Math.max(-(sceneDuration - 100), raw));
      setGesture({ itemId: item.id, kind: edge === "start" ? "trim-start" : "trim-end", deltaMs, deltaPx: 0 });
    };
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      setGesture(undefined);
      if (deltaMs) trimSceneByTimelineDelta(scene, edge, deltaMs);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end, { once: true });
  }

  function trimSceneByTimelineDelta(scene: VideoScene, edge: "start" | "end", deltaMs: number) {
    const timelineDuration = scene.timeline_end_ms - scene.timeline_start_ms;
    const sourceDuration = scene.source_end_ms - scene.source_start_ms;
    if (timelineDuration <= 0 || sourceDuration <= 0) return;
    const sourceDelta = deltaMs * sourceDuration / timelineDuration;
    const sourceStart = edge === "start" ? scene.source_start_ms + Math.max(0, sourceDelta) : scene.source_start_ms;
    const sourceEnd = edge === "end" ? scene.source_end_ms + Math.min(0, sourceDelta) : scene.source_end_ms;
    if (sourceEnd - sourceStart < 100) return;
    commit([{ type: "trim_scene", scene_id: scene.id, source_start_us: millisecondsToMicroseconds(sourceStart), source_end_us: millisecondsToMicroseconds(sourceEnd) }], `Trim ${scene.title}`);
  }

  function editClipWithKeyboard(scene: VideoScene, event: KeyboardEvent<HTMLButtonElement>) {
    const index = scenes.findIndex((candidate) => candidate.id === scene.id);
    if (event.altKey && !event.shiftKey && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      const toIndex = Math.max(0, Math.min(scenes.length - 1, index + (event.key === "ArrowRight" ? 1 : -1)));
      if (toIndex !== index) {
        event.preventDefault();
        commit([{ type: "reorder_scene", scene_id: scene.id, to_index: toIndex }], `Move ${scene.title}`);
      }
    } else if (!event.altKey && event.key.toLowerCase() === "s") {
      event.preventDefault();
      splitSelectedScene();
    }
  }

  function trimWithKeyboard(scene: VideoScene, edge: "start" | "end", event: KeyboardEvent<HTMLSpanElement>) {
    if (!['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    const step = event.shiftKey ? 1_000 : 100;
    const delta = edge === "start" ? (event.key === "ArrowRight" ? step : 0) : (event.key === "ArrowLeft" ? -step : 0);
    if (delta) trimSceneByTimelineDelta(scene, edge, delta);
  }

  function gestureStyle(item: VideoTimelineItem): CSSProperties {
    const active = gesture?.itemId === item.id ? gesture : undefined;
    if (!active) return itemStyle(item.start_ms, item.end_ms, duration);
    if (active.kind === "reorder") return { ...itemStyle(item.start_ms, item.end_ms, duration), transform: `translateX(${active.deltaPx}px)`, zIndex: 6 };
    const start = item.start_ms + (active.kind === "trim-start" ? active.deltaMs : 0);
    const end = item.end_ms + (active.kind === "trim-end" ? active.deltaMs : 0);
    return { ...itemStyle(start, end, duration), zIndex: 6 };
  }

  return (
    <section className={`video-timeline is-${mode}`} aria-label="Video timeline" data-track-count={trackOrder.length} data-timeline-mode={mode} data-timeline-zoomed={zoom > 1} style={height ? { height } : undefined}>
      <div className="video-timeline-resizer" role="separator" aria-label="Resize timeline" aria-orientation="horizontal" aria-valuemin={48} aria-valuemax={420} aria-valuenow={height ?? 210} tabIndex={onHeightChange ? 0 : -1} onPointerDown={beginHeightResize} onKeyDown={resizeHeightWithKeyboard}><GripHorizontal aria-hidden="true" size={14} /></div>
      <header className="video-timeline-toolbar">
        <div><button className="video-icon-button" type="button" aria-label="Split selected scene" disabled={!selectedSceneId || !onEditTimeline || editing} onClick={splitSelectedScene}><Scissors aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label="Zoom out timeline" disabled={zoom <= 0.75} onClick={() => setZoom((value) => Math.max(0.75, value - 0.25))}><Minus aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label="Zoom in timeline" disabled={zoom >= 2} onClick={() => setZoom((value) => Math.min(2, value + 0.25))}><Plus aria-hidden="true" size={14} /></button><Volume2 aria-hidden="true" size={14} /><span>{editing ? "Saving timeline…" : `Project timeline · ${Math.round(zoom * 100)}%`}</span></div>
        {/* Transport lives here so the preview pane is nothing but the canvas. */}
        <div className="video-timeline-transport" ref={onTransportHost} />
        <div><span>Source clock {formatVideoClock(timeline.source_clock_duration_ms, true)} · gaps preserved</span>{onModeChange ? <select aria-label="Timeline size" value={mode} onChange={(event) => onModeChange(event.target.value as VideoTimelineMode)}><option value="collapsed">Collapsed</option><option value="compact">Compact</option><option value="expanded">Expanded</option></select> : <span className="video-timeline-mode-label">Compact</span>}</div>
      </header>
      <div className="video-timeline-scroll">
        <div className="video-timeline-canvas" style={zoom <= 1 ? { width: "100%" } : { minWidth: `${Math.max(760, duration / 75) * zoom}px` }}>
          <div className="video-timeline-ruler" aria-hidden="true">{ticks.map((tick) => <span key={tick} style={{ left: `${(tick / duration) * 100}%` }}>{formatVideoClock(tick)}</span>)}</div>
          <input
            className="video-timeline-playhead-control"
            type="range"
            aria-label="Timeline playhead"
            min={0}
            max={duration}
            step={250}
            value={Math.min(playheadMs, duration)}
            aria-valuetext={formatVideoClock(playheadMs, true)}
            onChange={(event) => onPlayheadChange(Number(event.target.value))}
            onKeyDown={movePlayhead}
          />
          <div className="video-playhead-line" aria-hidden="true" style={{ left: `${(playheadMs / duration) * 100}%` }} />
          {trackOrder.map((kind) => {
            const track = timeline.tracks.find((candidate) => candidate.kind === kind) ?? { kind, items: [] };
            const meta = trackMeta[track.kind];
            const Icon = meta.icon;
            return <div className={`video-timeline-track is-${track.kind} ${selectedTrack === track.kind ? "is-active" : ""}`} key={track.kind} role="group" aria-label={`${meta.label} track`}>
              <button className="video-track-label" type="button" aria-pressed={selectedTrack === track.kind} onClick={() => setSelectedTrack(track.kind)}><Icon aria-hidden="true" size={14} /><span>{meta.label}</span></button>
              <div className="video-track-lane">
                {!track.items.length ? <span className="video-track-empty">No {meta.label.toLowerCase()} assets</span> : null}
                {track.items.map((item) => {
                  if (item.kind === "gap") return <span key={item.id} className="video-track-gap" role="note" aria-label={`${item.label}, ${formatVideoClock(item.end_ms - item.start_ms)} between clips`} style={itemStyle(item.start_ms, item.end_ms, duration)} />;
                  const scene = item.scene_id ? scenes.find((candidate) => candidate.id === item.scene_id) : undefined;
                  const selectedVideo = kind === "video" && scene?.id === selectedSceneId && Boolean(onEditTimeline);
                  const displayLabel = item.track === "visuals" ? "Imported image" : item.label;
                  return <div key={item.id} className={`video-track-item-shell ${item.scene_id === selectedSceneId ? "is-selected" : ""}`} style={gestureStyle(item)}>
                    <button
                      className={`video-track-item ${item.scene_id === selectedSceneId ? "is-selected" : ""} is-${item.kind}`}
                      type="button"
                      disabled={editing}
                      aria-label={`${displayLabel}, project ${formatVideoClock(item.start_ms)} to ${formatVideoClock(item.end_ms)}${item.source_start_ms !== undefined ? `, source ${formatVideoClock(item.source_start_ms)} to ${formatVideoClock(item.source_end_ms ?? item.source_start_ms)}` : ""}`}
                      title={`${displayLabel} · project ${formatVideoClock(item.start_ms)}–${formatVideoClock(item.end_ms)}${item.source_start_ms !== undefined ? ` · source ${formatVideoClock(item.source_start_ms)}–${formatVideoClock(item.source_end_ms ?? item.source_start_ms)}` : ""}. ${item.track === "video" ? "Drag to reorder; Alt+Arrow moves; S splits at the playhead." : "Select to inspect this layer."}`}
                      onPointerDown={(event) => beginReorder(item, event)}
                      onKeyDown={(event) => scene && editClipWithKeyboard(scene, event)}
                      onClick={() => item.scene_id && onSelectScene(item.scene_id)}
                    ><span>{displayLabel}</span></button>
                    {selectedVideo && scene ? <><span className="video-trim-handle is-start" role="separator" aria-label={`Trim start of ${scene.title}`} aria-orientation="vertical" tabIndex={0} onPointerDown={(event) => beginTrim(scene, item, "start", event)} onKeyDown={(event) => trimWithKeyboard(scene, "start", event)} /><span className="video-trim-handle is-end" role="separator" aria-label={`Trim end of ${scene.title}`} aria-orientation="vertical" tabIndex={0} onPointerDown={(event) => beginTrim(scene, item, "end", event)} onKeyDown={(event) => trimWithKeyboard(scene, "end", event)} /></> : null}
                  </div>;
                })}
              </div>
            </div>;
          })}
        </div>
      </div>
      <footer className="video-timeline-status"><span>Playhead {formatVideoClock(playheadMs, true)}</span><span>{scenes.length} scenes · {formatVideoClock(duration)} project duration</span><span>Arrow keys 1s · Shift 5s</span></footer>
    </section>
  );
}

export function millisecondsToMicroseconds(milliseconds: number): number {
  const microseconds = Math.round(milliseconds * 1_000);
  if (!Number.isFinite(milliseconds) || milliseconds < 0 || !Number.isSafeInteger(microseconds)) throw new Error("video.invalid_timestamp: Timeline time exceeds JavaScript's exact microsecond range.");
  return microseconds;
}

function snapMilliseconds(milliseconds: number): number {
  return Math.round(milliseconds / 100) * 100;
}

function clampTimelineHeight(value: number): number {
  return Math.max(48, Math.min(420, value));
}
