import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Archive, Check, CircleAlert, Download, Film, FolderOpen, LoaderCircle } from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties } from "react";
import { RowActionMenu } from "../ui";
import type { VideoArtifact, VideoProject } from "../../types/video";
import { formatVideoClock, formatVideoUpdatedAt, selectMasterArtifact } from "../../lib/videoState";
import { VideoProductionSteps } from "./VideoProductionSteps";
import { VideoTimeline } from "./VideoTimeline";
import { videoSourceForIdlePoster } from "../../lib/videoPlayback";
import { OpeningFrameVideo } from "./OpeningFrameVideo";

type ExportNotice = { tone: "status" | "error"; text: string };

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
  const masterDownload = useRef<HTMLAnchorElement>(null);
  const packageDownload = useRef<HTMLAnchorElement>(null);
  const [publishing, setPublishing] = useState(false);
  const [packageArtifact, setPackageArtifact] = useState<VideoArtifact>();
  const [notice, setNotice] = useState<ExportNotice | undefined>({ tone: "status", text: "Master export ready." });

  useEffect(() => {
    if (!notice) return;
    const timeout = window.setTimeout(() => setNotice(undefined), notice.tone === "error" ? 8_000 : 4_500);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  if (!master) return null;
  const masterLocalPath = master.local_path;

  async function publish() {
    setPublishing(true);
    setNotice(undefined);
    try {
      const artifact = await onPublishPackage();
      setPackageArtifact(artifact);
      setNotice({ tone: "status", text: "Publish package ready." });
    } catch (caught) {
      setNotice({ tone: "error", text: caught instanceof Error ? caught.message : String(caught) });
    } finally {
      setPublishing(false);
    }
  }

  async function openFolder() {
    if (!masterLocalPath) return;
    try {
      await revealItemInDir(masterLocalPath);
    } catch (caught) {
      setNotice({ tone: "error", text: caught instanceof Error ? caught.message : String(caught) });
    }
  }

  const overflowActions = [
    { label: "Download master", icon: <Download aria-hidden="true" size={13} />, onSelect: () => masterDownload.current?.click() },
    { label: "Open project", icon: <Film aria-hidden="true" size={13} />, onSelect: onEdit },
    { label: "Open output folder", icon: <FolderOpen aria-hidden="true" size={13} />, disabled: !masterLocalPath, onSelect: openFolder },
    packageArtifact?.url
      ? { label: "Download package", icon: <Archive aria-hidden="true" size={13} />, onSelect: () => packageDownload.current?.click() }
      : { label: "Publish package", icon: <Archive aria-hidden="true" size={13} />, disabled: publishing, onSelect: publish },
  ];

  return (
    <div className="video-export-complete">
      <div className="video-editor-toolbar">
        <div><span>Video Studio</span><h2 title={project.name}>{project.name}</h2></div>
        <div className="video-export-actions">
          <span className="video-exported-status"><Check aria-hidden="true" size={14} />Exported</span>
          <div className="video-export-direct-actions">
            <a ref={masterDownload} className="video-button is-secondary" href={master.url} download={master.download_name}><Download aria-hidden="true" size={13} /><span>Download</span></a>
            <button className="video-button is-secondary" type="button" onClick={onEdit}><Film aria-hidden="true" size={13} /><span>Open project</span></button>
            <button className="video-icon-button" type="button" aria-label="Open output folder" title={masterLocalPath ? "Open output folder" : "The local export path is unavailable"} disabled={!masterLocalPath} onClick={() => void openFolder()}><FolderOpen aria-hidden="true" size={14} /></button>
            {packageArtifact?.url
              ? <a ref={packageDownload} className="video-button is-primary" href={packageArtifact.url} download={packageArtifact.download_name}><Archive aria-hidden="true" size={13} /><span>Download package</span></a>
              : <button className="video-button is-primary" type="button" disabled={publishing} onClick={() => void publish()}>{publishing ? <LoaderCircle className="video-spin" aria-hidden="true" size={13} /> : <Archive aria-hidden="true" size={13} />}<span>{publishing ? "Building" : "Publish package"}</span></button>}
          </div>
          <div className="video-export-overflow"><RowActionMenu label="More export actions" actions={overflowActions} /></div>
        </div>
      </div>
      <VideoProductionSteps project={project} />
      <div className="video-export-layout">
        <aside className="video-export-scenes"><header><strong>Scenes ({project.manifest.scenes.length})</strong><span>{formatVideoClock(project.duration_ms)}</span></header><ol>{project.manifest.scenes.map((scene) => { const detail = `${scene.position}. ${scene.title}. Project ${formatVideoClock(scene.timeline_start_ms)} to ${formatVideoClock(scene.timeline_end_ms)}.`; return <li key={scene.id} aria-label={detail} title={detail}><span>{scene.position}</span><strong>{scene.title}</strong></li>; })}</ol><button className="video-button is-secondary" type="button" onClick={onEdit}>Edit scenes</button></aside>
        <main className="video-master-card" aria-label="Final master">
          <div className="video-master-main"><div className="video-master-frame" style={{ "--video-master-aspect": `${master.width ?? 9} / ${master.height ?? 16}` } as CSSProperties}><OpeningFrameVideo src={videoSourceForIdlePoster(master.url, master.poster_url ?? project.poster_url)} poster={master.poster_url ?? project.poster_url} controls playsInline preload={master.poster_url ?? project.poster_url ? "metadata" : "auto"} aria-label={`Final video: ${master.title}`} /></div></div>
          <footer><span className="video-master-title" title={master.title}>{master.title}</span><span>{formatVideoClock(master.duration_ms ?? 0)} · {master.width ?? "—"}×{master.height ?? "—"} · {master.codec ?? master.format.toUpperCase()}</span><span>Revision <strong>{project.revision}</strong></span><span>Saved <strong>{formatVideoUpdatedAt(project.updated_at)}</strong></span></footer>
        </main>
        <aside className="video-export-summary"><header>Export receipt</header><dl><div><dt>Format</dt><dd>{master.format.toUpperCase()}{master.codec ? ` · ${master.codec}` : ""}</dd></div><div><dt>Resolution</dt><dd>{master.width ?? "—"}×{master.height ?? "—"}</dd></div><div><dt>Duration</dt><dd>{formatVideoClock(master.duration_ms ?? 0)}</dd></div><div><dt>Frame rate</dt><dd>{master.frame_rate ? `${master.frame_rate} fps` : "—"}</dd></div><div><dt>File size</dt><dd>{master.file_size_bytes ? `${(master.file_size_bytes / 1_000_000).toFixed(1)} MB` : "—"}</dd></div><div><dt>Manifest</dt><dd>manifest.json</dd></div><div><dt>Checksum</dt><dd>{master.checksum ?? "Recorded"}</dd></div></dl></aside>
      </div>
      <VideoTimeline timeline={project.manifest.timeline} scenes={project.manifest.scenes} playheadMs={playheadMs} selectedSceneId={selectedSceneId} onPlayheadChange={onPlayheadChange} onSelectScene={onSelectScene} />
      {notice ? <div className={`video-export-toast is-${notice.tone}`} role={notice.tone === "error" ? "alert" : "status"}>{notice.tone === "error" ? <CircleAlert aria-hidden="true" size={14} /> : <Check aria-hidden="true" size={14} />}{notice.text}</div> : null}
    </div>
  );
}
