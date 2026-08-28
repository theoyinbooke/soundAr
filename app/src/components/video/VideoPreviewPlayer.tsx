import { Maximize2, Pause, Play, SkipBack, SkipForward, Volume2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { VideoArtifact, VideoScene } from "../../types/video";
import { formatVideoClock } from "../../lib/videoState";

export function VideoPreviewPlayer({
  sourceUrl,
  artifact,
  scene,
  projectDurationMs,
  playheadMs,
  onPlayheadChange,
}: {
  sourceUrl?: string;
  artifact?: VideoArtifact;
  scene?: VideoScene;
  projectDurationMs: number;
  playheadMs: number;
  onPlayheadChange: (milliseconds: number) => void;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [playing, setPlaying] = useState(false);
  const [volume, setVolume] = useState(0.72);
  const [aspectRatio, setAspectRatio] = useState("9 / 16");

  const mediaMilliseconds = artifact
    ? playheadMs
    : scene
      ? scene.source_start_ms + Math.max(0, Math.min(scene.timeline_end_ms - scene.timeline_start_ms, playheadMs - scene.timeline_start_ms))
      : playheadMs;

  useEffect(() => {
    if (videoRef.current && Math.abs(videoRef.current.currentTime * 1000 - mediaMilliseconds) > 1_100) {
      const durationSeconds = Number.isFinite(videoRef.current.duration) ? videoRef.current.duration : mediaMilliseconds / 1000;
      videoRef.current.currentTime = Math.min(durationSeconds, mediaMilliseconds / 1000);
    }
  }, [mediaMilliseconds]);

  useEffect(() => {
    if (!scene) return;
    setAspectRatio(scene.layout === "landscape" ? "16 / 9" : scene.layout === "square" ? "1" : "9 / 16");
  }, [scene?.id, scene?.layout]);

  useEffect(() => {
    if (!videoRef.current) return;
    videoRef.current.volume = volume;
    videoRef.current.muted = volume === 0;
  }, [volume]);

  async function togglePlayback() {
    const video = videoRef.current;
    if (!video) return;
    try {
      if (video.paused) await video.play(); else video.pause();
    } catch {
      setPlaying(false);
    }
  }

  function seek(milliseconds: number) {
    onPlayheadChange(Math.max(0, Math.min(projectDurationMs, milliseconds)));
  }

  async function enterFullscreen() {
    const frame = videoRef.current?.parentElement;
    if (frame?.requestFullscreen) await frame.requestFullscreen();
  }

  return (
    <section className="video-preview-player" aria-label="Fast video preview">
      <header><select aria-label="Preview aspect ratio" value={aspectRatio} onChange={(event) => setAspectRatio(event.target.value)}><option value="9 / 16">9:16 Portrait</option><option value="16 / 9">16:9 Landscape</option><option value="1">1:1 Square</option></select><span>{artifact ? "Rendered preview" : "Low-resolution proxy"}</span></header>
      <div className="video-preview-stage">
        <div className="video-portrait-frame" style={{ aspectRatio }}>
          <video ref={videoRef} src={artifact?.url ?? sourceUrl} playsInline loop preload="metadata" aria-label={artifact?.title ?? "Project proxy preview"} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onTimeUpdate={(event) => {
            if (!playing) return;
            const currentMs = event.currentTarget.currentTime * 1000;
            const timelineMs = artifact || !scene ? currentMs : scene.timeline_start_ms + (currentMs - scene.source_start_ms);
            onPlayheadChange(Math.max(0, Math.min(projectDurationMs, timelineMs)));
          }} />
          {scene?.captions_enabled ? <strong className={`video-caption-preview is-${scene.caption_style}`}>{scene.transcript}</strong> : null}
        </div>
      </div>
      <footer className="video-preview-controls">
        <div><button className="video-icon-button" type="button" aria-label="Previous scene" onClick={() => seek(scene?.timeline_start_ms ?? 0)}><SkipBack aria-hidden="true" size={14} /></button><button className="video-icon-button" type="button" aria-label={playing ? "Pause preview" : "Play preview"} onClick={() => void togglePlayback()}>{playing ? <Pause aria-hidden="true" size={15} /> : <Play aria-hidden="true" size={15} />}</button><button className="video-icon-button" type="button" aria-label="Next scene" onClick={() => seek(scene?.timeline_end_ms ?? projectDurationMs)}><SkipForward aria-hidden="true" size={14} /></button></div>
        <strong>{formatVideoClock(playheadMs, true)} <span>/ {formatVideoClock(projectDurationMs, true)}</span></strong>
        <div><Volume2 aria-hidden="true" size={14} /><input aria-label="Preview volume" type="range" min={0} max={1} step={0.05} value={volume} onChange={(event) => setVolume(Number(event.target.value))} /><button className="video-icon-button" type="button" aria-label="Full screen preview" onClick={() => void enterFullscreen()}><Maximize2 aria-hidden="true" size={14} /></button></div>
      </footer>
    </section>
  );
}
