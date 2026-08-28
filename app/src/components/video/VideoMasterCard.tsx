import { Archive, ChevronDown, Clapperboard, Download, ExternalLink, FileVideo2 } from "lucide-react";
import { useState } from "react";
import type { VideoProjectSummary } from "../../types/video";
import { videoSourceForIdlePoster, videoSourceWithFirstFrame } from "../../lib/videoPlayback";
import { OpeningFrameVideo } from "./OpeningFrameVideo";

function formatDuration(milliseconds = 0) {
  const seconds = Math.max(0, Math.round(milliseconds / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function formatDimensions(width?: number, height?: number) {
  return width && height ? `${width}×${height}` : "Local video";
}

export function VideoMasterCard({
  project,
  variant = "project",
  onOpen,
}: {
  project: VideoProjectSummary;
  variant?: "project" | "history";
  onOpen?: (projectId: string) => void;
}) {
  const [deliverablesOpen, setDeliverablesOpen] = useState(false);
  const master = project.master;
  const source = master?.url;
  const title = master?.title || project.name;
  const secondary = (project.deliverables ?? []).filter((artifact) => artifact.id !== master?.id);
  return (
    <article className={`video-library-card is-${variant}${master ? " has-master" : " is-draft"}`}>
      <div className="video-library-media">
        {master?.playable && source ? <OpeningFrameVideo aria-label={`Play ${title}`} controls playsInline preload={master.poster_url ?? project.poster_url ? "metadata" : "auto"} poster={master.poster_url ?? project.poster_url} src={videoSourceForIdlePoster(source, master.poster_url ?? project.poster_url)} /> : <div className="video-library-placeholder" aria-label={`${project.name} has no final master yet`}><Clapperboard aria-hidden="true" size={19} /><span>{project.status === "exported" ? "Master unavailable" : "Draft in progress"}</span></div>}
      </div>
      <div className="video-library-copy">
        <span className="section-label">{master ? "Primary video master" : "Video project"}</span>
        <h3>{title}</h3>
        <p>{project.scene_count} scene{project.scene_count === 1 ? "" : "s"} · {formatDuration(master?.duration_ms ?? project.duration_ms)}{master ? ` · ${formatDimensions(master.width, master.height)} · ${master.codec ?? master.format.toUpperCase()}` : ` · ${project.status.replaceAll("-", " ")}`}</p>
        {secondary.length ? <details className="video-library-deliverables" onToggle={(event) => setDeliverablesOpen(event.currentTarget.open)}><summary><span>{secondary.length} additional deliverable{secondary.length === 1 ? "" : "s"}</span><ChevronDown aria-hidden="true" size={11} /></summary>{deliverablesOpen ? <div>{secondary.map((artifact) => <article key={artifact.id}>
          {artifact.playable && artifact.url ? <video aria-label={`Play ${artifact.title}`} controls playsInline preload="metadata" src={videoSourceWithFirstFrame(artifact.url)} /> : <span className="video-library-file-icon">{artifact.role === "publish-package" ? <Archive aria-hidden="true" size={13} /> : <FileVideo2 aria-hidden="true" size={13} />}</span>}
          <span><strong>{artifact.title}</strong><small>{artifact.role === "publish-package" ? "Publish ZIP" : `${formatDuration(artifact.duration_ms)} · ${formatDimensions(artifact.width, artifact.height)}`}</small></span>
          {artifact.url ? <a aria-label={`Download ${artifact.title}`} download={artifact.download_name} href={artifact.url}><Download aria-hidden="true" size={11} /></a> : null}
        </article>)}</div> : null}</details> : null}
        <div className="video-library-actions">
          <button className="button button-secondary" type="button" onClick={() => onOpen?.(project.id)}><ExternalLink aria-hidden="true" size={12} />Open in Video Studio</button>
          {master?.url ? <a className="button button-primary" aria-label={`Download ${title}`} download={master.download_name ?? `${project.id}-master.mp4`} href={master.url}><Download aria-hidden="true" size={12} />Download MP4</a> : null}
        </div>
      </div>
    </article>
  );
}

export function sortVideoProjectsForLibrary(projects: VideoProjectSummary[]) {
  return [...projects].sort((left, right) => {
    if (Boolean(left.master) !== Boolean(right.master)) return left.master ? -1 : 1;
    return Date.parse(right.updated_at) - Date.parse(left.updated_at);
  });
}
