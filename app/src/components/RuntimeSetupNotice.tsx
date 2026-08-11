import { CircleAlert, Download, LoaderCircle } from "lucide-react";
import { useState } from "react";
import { setupPythonRuntime } from "../lib/bridge";

export function RuntimeSetupNotice({ onReady }: { onReady: () => Promise<void> }) {
  const [isInstalling, setIsInstalling] = useState(false);
  const [progress, setProgress] = useState("Ready to install the local inference runtime.");
  const [error, setError] = useState<string>();

  async function install() {
    if (isInstalling) return;
    setIsInstalling(true);
    setError(undefined);
    try {
      await setupPythonRuntime(setProgress);
      setProgress("Local inference runtime is ready.");
      await onReady();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setIsInstalling(false);
    }
  }

  return (
    <section className="runtime-setup-notice" aria-label="Runtime setup" aria-live="polite">
      <div className="runtime-setup-icon">
        {isInstalling ? <LoaderCircle className="spin" aria-hidden="true" size={17} /> : <CircleAlert aria-hidden="true" size={17} />}
      </div>
      <div className="runtime-setup-copy">
        <strong>Local runtime setup required</strong>
        <span>{error ?? progress}</span>
        {isInstalling ? <div className="runtime-setup-meter" aria-hidden="true"><i /></div> : null}
      </div>
      <div className="runtime-setup-actions">
        <small>One-time download · several GB</small>
        <button className="button button-primary" type="button" onClick={install} disabled={isInstalling}>
          {isInstalling ? <LoaderCircle className="spin" aria-hidden="true" size={14} /> : <Download aria-hidden="true" size={14} />}
          {isInstalling ? "Setting up..." : error ? "Retry setup" : "Set up runtime"}
        </button>
      </div>
    </section>
  );
}
