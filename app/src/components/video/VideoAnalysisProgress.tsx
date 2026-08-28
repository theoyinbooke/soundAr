import { Check, FileVideo2, LoaderCircle, ScanText, Scissors, Video } from "lucide-react";
import type { VideoJob, VideoProject } from "../../types/video";

export function VideoAnalysisProgress({ project, job, onCancel, onResume }: { project: VideoProject; job?: VideoJob; onCancel: () => void; onResume: () => void }) {
  const progress = Math.round((job?.progress ?? 0) * 100);
  const stages = [
    { label: "Conform source and proxy", threshold: 18, icon: FileVideo2 },
    { label: "Transcribe on source clock", threshold: 46, icon: ScanText },
    { label: "Find candidate clips", threshold: 72, icon: Scissors },
    { label: "Prepare review", threshold: 100, icon: Video },
  ];
  return (
    <section className="video-analysis-state" aria-labelledby="video-analysis-title">
      <div className="video-analysis-heading"><LoaderCircle className="video-spin" aria-hidden="true" size={22} /><div><h2 id="video-analysis-title">Analyzing {project.manifest.source.display_name}</h2><p>{job?.detail ?? "Preparing the durable local analysis job…"}</p></div></div>
      <div className="video-analysis-progress" role="progressbar" aria-label="Video analysis progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress}><span style={{ width: `${progress}%` }} /></div>
      <ol className="video-analysis-stages">
        {stages.map((stage) => {
          const complete = progress >= stage.threshold;
          const active = !complete && progress >= Math.max(0, stage.threshold - 30);
          const Icon = stage.icon;
          return <li key={stage.label} className={complete ? "is-complete" : active ? "is-active" : ""}><span>{complete ? <Check aria-hidden="true" size={14} /> : <Icon aria-hidden="true" size={14} />}</span><strong>{stage.label}</strong><small>{complete ? "Ready" : active ? "Working" : "Waiting"}</small></li>;
        })}
      </ol>
      <div className="video-analysis-partial"><video src={project.manifest.source.preview_url} muted playsInline preload="metadata" aria-label="Playable low-resolution source proxy" /><div><strong>Low-resolution proxy</strong><span>{project.manifest.artifacts.some((artifact) => artifact.role === "proxy") ? "Playable while analysis continues" : "Publishing atomically…"}</span></div></div>
      <footer><span role="status" aria-live="polite">{progress}% · {job?.detail ?? "Ready to resume"}</span>{job ? <button className="video-button is-secondary" type="button" onClick={onCancel}>Cancel analysis</button> : <button className="video-button is-primary" type="button" onClick={onResume}>Resume analysis</button>}</footer>
    </section>
  );
}
