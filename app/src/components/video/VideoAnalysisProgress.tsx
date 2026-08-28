import { Check, FileVideo2, LoaderCircle, ScanText, Scissors, Video } from "lucide-react";
import type { VideoJob, VideoProject } from "../../types/video";

export function VideoAnalysisProgress({ project, job, onCancel, onResume }: { project: VideoProject; job?: VideoJob; onCancel: () => void; onResume?: () => void }) {
  const visibleJob = job ?? project.workflow_job;
  const recoverable = project.recoverable_job;
  const phase = visibleJob?.phase ?? recoverable?.phase ?? "analyze";
  const preparingSource = phase === "source";
  const active = visibleJob && ["queued", "preparing", "running"].includes(visibleJob.status);
  const progress = Math.round((visibleJob?.progress ?? 0) * 100);
  const stages = preparingSource
    ? [
        { label: "Create durable project", threshold: 8, icon: FileVideo2 },
        { label: "Generate local narration", threshold: 58, icon: ScanText },
        { label: "Register soundAr audio", threshold: 82, icon: Scissors },
        { label: "Prepare the timeline", threshold: 100, icon: Video },
      ]
    : [
        { label: "Conform source and proxy", threshold: 18, icon: FileVideo2 },
        { label: "Transcribe on source clock", threshold: 46, icon: ScanText },
        { label: "Find candidate clips", threshold: 72, icon: Scissors },
        { label: "Prepare review", threshold: 100, icon: Video },
      ];
  const actionLabel = preparingSource ? "video creation" : "analysis";
  return (
    <section className="video-analysis-state" aria-labelledby="video-analysis-title">
      <div className="video-analysis-heading"><LoaderCircle className={active ? "video-spin" : ""} aria-hidden="true" size={22} /><div><h2 id="video-analysis-title">{preparingSource ? `Preparing ${project.name}` : `Analyzing ${project.manifest.source.display_name}`}</h2><p>{visibleJob?.detail ?? (preparingSource ? "Preparing the durable prompt-to-video task…" : "Preparing the durable local analysis job…")}</p></div></div>
      <div className="video-analysis-progress" role="progressbar" aria-label={preparingSource ? "Video creation progress" : "Video analysis progress"} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress}><span style={{ width: `${progress}%` }} /></div>
      <ol className="video-analysis-stages">
        {stages.map((stage) => {
          const complete = progress >= stage.threshold;
          const active = !complete && progress >= Math.max(0, stage.threshold - 30);
          const Icon = stage.icon;
          return <li key={stage.label} className={complete ? "is-complete" : active ? "is-active" : ""}><span>{complete ? <Check aria-hidden="true" size={14} /> : <Icon aria-hidden="true" size={14} />}</span><strong>{stage.label}</strong><small>{complete ? "Ready" : active ? "Working" : "Waiting"}</small></li>;
        })}
      </ol>
      {!preparingSource ? <div className="video-analysis-partial"><video src={project.manifest.source.preview_url} muted playsInline preload="metadata" aria-label="Playable low-resolution source proxy" /><div><strong>Low-resolution proxy</strong><span>{project.manifest.artifacts.some((artifact) => artifact.role === "proxy") ? "Playable while analysis continues" : "Publishing atomically…"}</span></div></div> : null}
      <footer><span role="status" aria-live="polite">{progress}% · {visibleJob?.detail ?? `Waiting for a durable ${actionLabel} task`}</span>{active ? <button className="video-button is-secondary" type="button" onClick={onCancel}>Cancel {actionLabel}</button> : recoverable && onResume ? <button className="video-button is-primary" type="button" onClick={onResume}>Resume {actionLabel}</button> : null}</footer>
    </section>
  );
}
