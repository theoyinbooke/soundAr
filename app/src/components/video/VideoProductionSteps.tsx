import { Check } from "lucide-react";
import { videoProjectReadiness } from "../../lib/videoState";
import type { VideoProject } from "../../types/video";

const productionSteps = [
  { key: "source", label: "Source" },
  { key: "analyzed", label: "Analyze" },
  { key: "reviewed", label: "Review" },
  { key: "previewed", label: "Preview" },
  { key: "exported", label: "Export" },
] as const;

export function VideoProductionSteps({ project }: { project: VideoProject }) {
  const readiness = videoProjectReadiness(project);
  const current = productionSteps.find((step) => !readiness[step.key])?.key;

  return (
    <nav className="video-production-steps" aria-label="Video production progress">
      {productionSteps.map((step, index) => {
        const complete = readiness[step.key];
        const isCurrent = current === step.key;
        return (
          <span
            key={step.key}
            className={complete ? "is-complete" : isCurrent ? "is-current" : undefined}
            aria-current={isCurrent ? "step" : undefined}
            data-status={complete ? "complete" : isCurrent ? "current" : "upcoming"}
          >
            <i>{complete ? <Check aria-hidden="true" size={11} /> : index + 1}</i>
            {step.label}
          </span>
        );
      })}
    </nav>
  );
}
