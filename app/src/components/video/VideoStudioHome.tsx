import { ArrowRight, Film, Link2, Mic2, Upload } from "lucide-react";
import type { VideoProjectSummary, VideoStudioEntry } from "../../types/video";
import { formatVideoClock, formatVideoUpdatedAt } from "../../lib/videoState";

export function VideoStudioHome({
  projects,
  loading,
  onEntry,
  onOpenProject,
}: {
  projects: VideoProjectSummary[];
  loading: boolean;
  onEntry: (entry: VideoStudioEntry) => void;
  onOpenProject: (projectId: string) => void;
}) {
  const entries: Array<{ key: VideoStudioEntry; title: string; description: string; icon: typeof Link2 }> = [
    { key: "link", title: "Import link", description: "Import one authorized video URL.", icon: Link2 },
    { key: "upload", title: "Upload video", description: "Analyze a local video from your device.", icon: Upload },
    { key: "prompt", title: "Start from prompt or audio", description: "Create from an idea or existing soundAr audio.", icon: Mic2 },
  ];

  return (
    <div className="video-studio-home">
      <div className="video-entry-strip" role="group" aria-label="Start a video project">
        {entries.map((entry) => {
          const Icon = entry.icon;
          return <button key={entry.key} type="button" onClick={() => onEntry(entry.key)}><span className="video-entry-icon"><Icon aria-hidden="true" size={21} /></span><span><strong>{entry.title}</strong><small>{entry.description}</small></span></button>;
        })}
      </div>

      <section className="video-recent-projects" aria-labelledby="recent-video-projects-title">
        <h2 id="recent-video-projects-title">Recent video projects</h2>
        {loading ? <div className="video-project-loading" role="status">Loading local video projects…</div> : projects.length ? <div className="video-project-table" role="group" aria-label="Recent video projects">
          <div className="video-project-row is-heading" aria-hidden="true"><span>Name</span><span>Updated</span><span>Duration</span><span /></div>
          {projects.map((project) => <button className="video-project-row" type="button" key={project.id} onClick={() => onOpenProject(project.id)}>
            <span><i><Film aria-hidden="true" size={14} /></i><strong>{project.name}</strong></span>
            <span>{formatVideoUpdatedAt(project.updated_at)}</span>
            <span>{formatVideoClock(project.duration_ms)}</span>
            <span><ArrowRight aria-hidden="true" size={14} /></span>
          </button>)}
        </div> : <div className="video-project-empty"><Film aria-hidden="true" size={20} /><strong>No video projects yet</strong><span>Choose one of the three starting points above.</span></div>}
      </section>
    </div>
  );
}
