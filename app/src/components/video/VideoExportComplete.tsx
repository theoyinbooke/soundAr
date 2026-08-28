import { Archive, Check, Download, FolderOpen, LoaderCircle, Play } from "lucide-react";
import { useState } from "react";
import type { VideoArtifact, VideoProject } from "../../types/video";
import { formatVideoClock, formatVideoUpdatedAt, selectMasterArtifact } from "../../lib/videoState";
import { VideoTimeline } from "./VideoTimeline";

export function VideoExportComplete({
  project,
  playheadMs,
  selectedSceneId,
  onPlayheadChange,
  onSelectScene,
  onEdit,
  onPublishPackage,
}: {
  project: VideoProject;
  playheadMs: number;
  selectedSceneId?: string;
  onPlayheadChange: (milliseconds: number) => void;
  onSelectScene: (sceneId: string) => void;
  onEdit: () => void;
  onPublishPackage: () => Promise<VideoArtifact>;
}) {
  const master = selectMasterArtifact(project);
  const [publishing, setPublishing] = useState(false);
  const [packageArtifact, setPackageArtifact] = useState<VideoArtifact>();
  const [error, setError] = useState<string>();
  if (!master) return null;

  async function publish() {
    setPublishing(true);
    setError(undefined);
    try { setPackageArtifact(await onPublishPackage()); }
    catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setPublishing(false); }
  }

  return (
    <div className="video-export-complete">
      <div className="video-editor-toolbar"><div><span>Video Studio</span><h2>{project.name}</h2></div><div><span className="video-exported-status"><Check aria-hidden="true" size={14} />Exported</span></div></div>
      <nav className="video-production-steps" aria-label="Video production progress">{["Source", "Analyze", "Review", "Preview", "Export"].map((label) => <span key={label} className="is-complete"><i><Check aria-hidden="true" size={11} /></i>{label}</span>)}</nav>
      <div className="video-export-layout">
        <aside className="video-export-scenes"><header><strong>Scenes ({project.manifest.scenes.length})</strong><span>{formatVideoClock(project.duration_ms)}</span></header><ol>{project.manifest.scenes.map((scene) => <li key={scene.id}><span>{scene.position}</span><div><strong>{scene.title}</strong><small>{formatVideoClock(scene.timeline_start_ms)}</small></div></li>)}</ol><button className="video-button is-secondary" type="button" onClick={onEdit}>Edit scenes</button></aside>
        <main className="video-master-card" aria-labelledby="export-complete-title">
          <header><Check aria-hidden="true" size={20} /><div><h2 id="export-complete-title">Export complete</h2><p>Local render finished successfully.</p></div></header>
          <div className="video-master-main"><div className="video-master-frame"><video src={master.url} controls playsInline preload="metadata" aria-label={`Final video: ${master.title}`} /><span aria-hidden="true"><Play size={23} /></span></div><div className="video-master-copy"><h3>{master.title}</h3><p>{formatVideoClock(master.duration_ms ?? 0)} · {master.width}×{master.height} (9:16) · {master.codec} · Hardware render</p><span>Rendered on this machine</span><div><a className="video-button is-secondary" href={master.url} download={master.download_name}><Download aria-hidden="true" size={14} />Download master</a><button className="video-button is-secondary" type="button" onClick={onEdit}><FolderOpen aria-hidden="true" size={14} />Open project</button><button className="video-button is-primary" type="button" disabled={publishing} onClick={() => void publish()}>{publishing ? <LoaderCircle className="video-spin" aria-hidden="true" size={14} /> : <Archive aria-hidden="true" size={14} />}{publishing ? "Building package" : "Publish package"}</button></div></div></div>
          <footer><span>Manifest <strong>manifest.json</strong></span><span>Checksum <strong>{master.checksum}</strong></span><span>Cache reuse <strong>82%</strong></span></footer>
          {packageArtifact ? <div className="video-package-ready" role="status"><Check aria-hidden="true" size={14} /><span>{packageArtifact.download_name} is ready.</span>{packageArtifact.url ? <a href={packageArtifact.url} download={packageArtifact.download_name}>Download package</a> : null}</div> : null}
          {error ? <div className="video-inline-error" role="alert">{error}</div> : null}
        </main>
        <aside className="video-export-summary"><header>Export summary</header><dl><div><dt>Format</dt><dd>MP4 ({master.codec})</dd></div><div><dt>Resolution</dt><dd>{master.width}×{master.height} (9:16)</dd></div><div><dt>Duration</dt><dd>{formatVideoClock(master.duration_ms ?? 0)}</dd></div><div><dt>Frame rate</dt><dd>{master.frame_rate} fps</dd></div><div><dt>Audio</dt><dd>AAC · 48 kHz · Stereo</dd></div><div><dt>Render mode</dt><dd>Hardware (NVENC)</dd></div><div><dt>File size</dt><dd>{((master.file_size_bytes ?? 0) / 1_000_000).toFixed(1)} MB</dd></div></dl></aside>
      </div>
      <VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={playheadMs} selectedSceneId={selectedSceneId} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} />
      <footer className="video-project-status"><span>Project duration <strong>{formatVideoClock(project.duration_ms)}</strong></span><span>Source <strong>{formatVideoClock(project.manifest.source.duration_ms)}</strong></span><span>Revision <strong>{project.revision}</strong></span><span>Saved <strong>{formatVideoUpdatedAt(project.updated_at)}</strong></span></footer>
    </div>
  );
}
