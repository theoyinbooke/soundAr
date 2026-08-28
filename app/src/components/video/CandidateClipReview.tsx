import { Check, Clock3, Play, Scissors } from "lucide-react";
import type { VideoProject } from "../../types/video";
import { candidateDuration, formatVideoClock } from "../../lib/videoState";

export function CandidateClipReview({
  project,
  selectedIds,
  onToggle,
  onContinue,
  onBack,
}: {
  project: VideoProject;
  selectedIds: string[];
  onToggle: (candidateId: string) => void;
  onContinue: () => void;
  onBack: () => void;
}) {
  const selected = new Set(selectedIds);
  const total = project.manifest.candidates.reduce((duration, candidate) => duration + (selected.has(candidate.id) ? candidateDuration(candidate) : 0), 0);
  return (
    <section className="video-review-state" aria-labelledby="candidate-review-title">
      <header className="video-review-heading"><div><h2 id="candidate-review-title">Review candidate clips</h2><p>Source-clock timings and silent gaps remain attached to every selection.</p></div><div><span>{selected.size} selected</span><strong>{formatVideoClock(total)} content</strong></div></header>
      <div className="video-review-grid">
        <div className="video-candidate-panel">
          <div className="video-source-summary"><div><span>Source</span><strong>{project.manifest.source.display_name}</strong></div><span><Clock3 aria-hidden="true" size={13} />{formatVideoClock(project.manifest.source.duration_ms)}</span></div>
          <div className="video-candidate-list" role="list" aria-label="Candidate clips">
            {project.manifest.candidates.map((candidate) => <label className={`video-candidate-row ${selected.has(candidate.id) ? "is-selected" : ""}`} key={candidate.id} role="listitem">
              <input type="checkbox" checked={selected.has(candidate.id)} onChange={() => onToggle(candidate.id)} aria-label={`Include ${candidate.title}`} />
              <span className="video-candidate-rank">{candidate.rank}</span>
              <span className="video-candidate-copy"><small>{formatVideoClock(candidate.source_start_ms)} – {formatVideoClock(candidate.source_end_ms)}</small><strong>{candidate.title}</strong><span>{candidate.transcript}</span></span>
              <span className="video-candidate-score">Score {candidate.score}</span>
            </label>)}
          </div>
          <footer><span>{selected.size} selected · {formatVideoClock(total)} total</span><button className="video-button is-primary" type="button" disabled={!selected.size} onClick={onContinue}><Scissors aria-hidden="true" size={14} />Add to timeline</button></footer>
        </div>
        <div className="video-review-preview">
          <span className="video-section-label">Source preview</span>
          <div className="video-review-frame"><video src={project.manifest.source.preview_url} muted loop playsInline controls preload="metadata" aria-label="Candidate source preview" /><span aria-hidden="true"><Play size={20} /></span></div>
          <div className="video-review-receipt"><Check aria-hidden="true" size={15} /><span><strong>Local analysis complete</strong><small>{project.manifest.transcript.length} source-clock transcript segments · proxy cached</small></span></div>
        </div>
      </div>
      <footer className="video-review-actions"><button className="video-button is-secondary" type="button" onClick={onBack}>Back to projects</button><span role="status" aria-live="polite">Choose the moments that belong in this edit.</span></footer>
    </section>
  );
}
