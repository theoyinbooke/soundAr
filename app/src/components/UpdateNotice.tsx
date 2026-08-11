import { Download, ExternalLink, LoaderCircle, RefreshCw, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import type { DownloadEvent, Update } from "@tauri-apps/plugin-updater";
import { useState } from "react";
import type { BootstrapState } from "../types";

export function UpdateNotice({
  update,
  installKind,
  onDismiss,
}: {
  update: Update;
  installKind: BootstrapState["install_kind"];
  onDismiss: () => void;
}) {
  const [isInstalling, setIsInstalling] = useState(false);
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState<string>();
  const canInstall = installKind === "appimage";

  async function applyUpdate() {
    setError(undefined);
    if (!canInstall) {
      const version = update.version.replace(/^v/, "");
      await openUrl(`https://github.com/theoyinbooke/soundAr/releases/tag/v${version}`);
      return;
    }

    setIsInstalling(true);
    let downloaded = 0;
    let total = 0;
    try {
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress") downloaded += event.data.chunkLength;
        if (total > 0) setProgress(Math.min(100, Math.round((downloaded / total) * 100)));
        if (event.event === "Finished") setProgress(100);
      });
      await relaunch();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
      setIsInstalling(false);
    }
  }

  return (
    <section className="app-update-notice" aria-label="Application update" aria-live="polite">
      <div className="app-update-icon">
        {isInstalling ? <LoaderCircle className="spin" aria-hidden="true" size={17} /> : <RefreshCw aria-hidden="true" size={17} />}
      </div>
      <div className="app-update-copy">
        <strong>soundAr {update.version} is available</strong>
        <span>{error ?? (isInstalling ? `Downloading signed update · ${progress}%` : canInstall ? "Ready to install and restart." : "Open the release to update the Debian package.")}</span>
        {isInstalling ? <div className="app-update-meter"><i style={{ width: `${progress}%` }} /></div> : null}
      </div>
      <div className="app-update-actions">
        <button className="button button-primary" type="button" onClick={applyUpdate} disabled={isInstalling}>
          {isInstalling ? <LoaderCircle className="spin" aria-hidden="true" size={14} /> : canInstall ? <Download aria-hidden="true" size={14} /> : <ExternalLink aria-hidden="true" size={14} />}
          {isInstalling ? "Installing..." : canInstall ? "Install update" : "Open release"}
        </button>
        <button className="icon-button" type="button" title="Remind me later" onClick={onDismiss} disabled={isInstalling}>
          <X aria-hidden="true" size={14} />
        </button>
      </div>
    </section>
  );
}
