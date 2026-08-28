import { Captions, Minus, Music2, Plus, Scissors, Video, Volume2, Waves } from "lucide-react";
import { useState, type CSSProperties, type KeyboardEvent } from "react";
import type { VideoScene, VideoTimelineManifest, VideoTimelineTrackKind } from "../../types/video";
import { formatVideoClock } from "../../lib/videoState";

const trackMeta: Record<VideoTimelineTrackKind, { label: string; icon: typeof Video }> = {
  video: { label: "Video", icon: Video },
  captions: { label: "Captions", icon: Captions },
  voice: { label: "Voice", icon: Waves },
  music: { label: "Music", icon: Music2 },
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
  onSplitScene,
}: {
  timeline: VideoTimelineManifest;
  scenes: VideoScene[];
  playheadMs: number;
  selectedSceneId?: string;
  onPlayheadChange: (milliseconds: number) => void;
  onSelectScene: (sceneId: string) => void;
  onSplitScene?: (sceneId: string, atMs: number) => void;
}) {
  const duration = Math.max(1, timeline.duration_ms);
  const [zoom, setZoom] = useState(1);
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

  return (
    <section className="video-timeline" aria-label="Video timeline">
      <header className="video-timeline-toolbar">
        <div><button className="video-icon-button" type="button" aria-label="Split selected scene" disabled={!selectedSceneId || !onSplitScene} onClick={() => selectedSceneId && onSplitScene?.(selectedSceneId, playheadMs)}><Scissors aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label="Zoom out timeline" disabled={zoom <= 0.75} onClick={() => setZoom((value) => Math.max(0.75, value - 0.25))}><Minus aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label="Zoom in timeline" disabled={zoom >= 2} onClick={() => setZoom((value) => Math.min(2, value + 0.25))}><Plus aria-hidden="true" size={14} /></button><Volume2 aria-hidden="true" size={14} /><span>Project timeline · {Math.round(zoom * 100)}%</span></div>
        <span>Source clock {formatVideoClock(timeline.source_clock_duration_ms, true)} · gaps preserved</span>
      </header>
      <div className="video-timeline-scroll">
        <div className="video-timeline-canvas" style={{ minWidth: `${Math.max(760, duration / 75) * zoom}px` }}>
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
          {timeline.tracks.map((track) => {
            const meta = trackMeta[track.kind];
            const Icon = meta.icon;
            return <div className={`video-timeline-track is-${track.kind}`} key={track.kind} role="group" aria-label={`${meta.label} track`}>
              <div className="video-track-label"><Icon aria-hidden="true" size={14} /><span>{meta.label}</span></div>
              <div className="video-track-lane">
                {track.items.map((item) => item.kind === "gap" ? <span key={item.id} className="video-track-gap" role="note" aria-label={`${item.label}, ${formatVideoClock(item.end_ms - item.start_ms)} between clips`} style={itemStyle(item.start_ms, item.end_ms, duration)} /> : <button
                  key={item.id}
                  className={`video-track-item ${item.scene_id === selectedSceneId ? "is-selected" : ""} is-${item.kind}`}
                  type="button"
                  style={itemStyle(item.start_ms, item.end_ms, duration)}
                  aria-label={`${item.label}, project ${formatVideoClock(item.start_ms)} to ${formatVideoClock(item.end_ms)}${item.source_start_ms !== undefined ? `, source ${formatVideoClock(item.source_start_ms)} to ${formatVideoClock(item.source_end_ms ?? item.source_start_ms)}` : ""}`}
                  onClick={() => item.scene_id && onSelectScene(item.scene_id)}
                ><span>{item.label}</span>{item.source_start_ms !== undefined ? <small>{formatVideoClock(item.source_start_ms)} source</small> : null}</button>)}
              </div>
            </div>;
          })}
        </div>
      </div>
      <footer className="video-timeline-status"><span>Playhead {formatVideoClock(playheadMs, true)}</span><span>{scenes.length} scenes · {formatVideoClock(duration)} project duration</span><span>Arrow keys 1s · Shift 5s</span></footer>
    </section>
  );
}
